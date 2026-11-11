# Makefile - front door for building, testing and installing openrc-rs
#
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261106 - Document OPENRC_BRANDING (distro name in --version) - Claude - scottfk
# 20261106 - Build hybrid librc.so.1 (Rust + weakened upstream C) - Claude - scottfk
# 20261107 - librc.so.1 is all Rust: drop the upstream C half - Claude - scottfk
# 20261110 - PAM=yes (upstream's -Dpam=true): PAM sessions, pam_openrc.so; `make pam` for the tests - Claude - scottfk
# 20261111 - Document OPENRC_NAME (program name in banners and --version) - Claude - scottfk
#
# Cargo builds everything except the final link of the C ABI libraries
# (the variadic printf-style functions are C, see crates/capi-einfo/), so
# that step and installation live here.
#
#   make              release build: multi-call binary + libeinfo.so.1 + librc.so.1
#   make check        build the C reference if needed, run every test
#                     (unit + differential), failing if the reference is missing
#   make install DESTDIR=/some/stage   install with applet symlinks
#   make PAM=yes ...  with PAM (upstream's -Dpam=true, Gentoo's USE=pam):
#                     daemon sessions, PAM in openrc-user, and pam_openrc.so
#                     (installed in PAMLIBDIR)
#   make pam          the PAM build the tests use, in target/pam/
#
# Install layout variables match upstream's meson defaults.  Set
# OPENRC_BRANDING="Gentoo Linux" (upstream's -Dbranding) to match a distro
# build's --version output; cargo rebuilds when it changes.
# OPENRC_NAME="OpenRC-rs" replaces "OpenRC" in the boot banners and
# --version (not an upstream option; unset, output matches C).

CARGO   ?= cargo
CC      ?= cc
CFLAGS  ?= -O2 -g
PROFILE ?= release

PREFIX     ?= /usr
LIBDIR     ?= $(PREFIX)/lib64
INCLUDEDIR ?= $(PREFIX)/include
BINDIR     ?= $(PREFIX)/bin
SBINDIR    ?= $(PREFIX)/sbin
RC_LIBEXECDIR ?= $(PREFIX)/libexec/rc
PAMLIBDIR  ?= /lib64/security
DESTDIR    ?=
PAM        ?= no

CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)
PAM_FEATURE := $(if $(filter yes,$(PAM)),--features pam,)
TARGET_DIR := target/$(if $(filter release,$(PROFILE)),release,debug)
CAPI_DIR   := target/capi
BIN        := $(TARGET_DIR)/openrc-rs

# From `cargo rustc -- --print native-static-libs`.
RUST_NATIVE_LIBS := -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc

# Exports implemented in Rust; -u makes the linker pull them out of the
# static archive (nothing in the C shim references them).
EINFO_RUST_EXPORTS := ebracket ecolor eindent eindentv eoutdent eoutdentv eprefix

.PHONY: all build capi pam reference test check difftest clippy fmt install clean

all: build capi

build:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-applets $(PAM_FEATURE)
ifeq ($(PAM),yes)
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-pam-openrc
endif

# The PAM variant for the differential tests (against the reference's
# build-pam/), apart from the normal build so features don't mix.
pam:
	$(CARGO) build -p openrc-applets --features pam --target-dir target/pam
	$(CARGO) build -p openrc-pam-openrc --target-dir target/pam

$(TARGET_DIR)/libopenrc_capi_einfo.a: FORCE
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-capi-einfo

$(CAPI_DIR)/libeinfo.so.1: $(TARGET_DIR)/libopenrc_capi_einfo.a crates/capi-einfo/shim/einfo-shim.c crates/capi-einfo/libeinfo.map
	@mkdir -p $(CAPI_DIR)
	$(CC) $(CFLAGS) -fPIC -shared -o $@ \
		-Icrates/capi-einfo/include \
		-Wl,-soname,libeinfo.so.1 -Wl,--gc-sections -Wl,--as-needed \
		-Wl,--version-script=crates/capi-einfo/libeinfo.map \
		$(addprefix -Wl$(,)-u$(,),$(EINFO_RUST_EXPORTS)) \
		crates/capi-einfo/shim/einfo-shim.c \
		$(TARGET_DIR)/libopenrc_capi_einfo.a $(RUST_NATIVE_LIBS)
	ln -sf libeinfo.so.1 $(CAPI_DIR)/libeinfo.so

# --- librc.so.1: all Rust.  -u for each export pulls it out of the static
# archive; the version script keeps exactly upstream's symbols.
RC_DIR     := crates/capi-rc
RC_EXPORTS := $(shell cat $(RC_DIR)/exports.txt)

$(TARGET_DIR)/libopenrc_capi_rc.a: FORCE
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-capi-rc

$(CAPI_DIR)/librc.map: $(RC_DIR)/exports.txt
	@mkdir -p $(CAPI_DIR)
	{ echo '{ global:'; sed 's/$$/;/' $<; echo 'local: *; };'; } > $@

$(CAPI_DIR)/librc.so.1: $(TARGET_DIR)/libopenrc_capi_rc.a $(CAPI_DIR)/librc.map
	$(CC) $(CFLAGS) -fPIC -shared -o $@ \
		-Wl,-soname,librc.so.1 -Wl,--as-needed \
		-Wl,--version-script=$(CAPI_DIR)/librc.map \
		$(addprefix -Wl$(,)-u$(,),$(RC_EXPORTS)) \
		$(TARGET_DIR)/libopenrc_capi_rc.a $(RUST_NATIVE_LIBS)
	ln -sf librc.so.1 $(CAPI_DIR)/librc.so

capi: $(CAPI_DIR)/libeinfo.so.1 $(CAPI_DIR)/librc.so.1

reference:
	tools/build-reference.sh

test: capi pam
	$(CARGO) test

check: reference capi pam
	OPENRC_REF_REQUIRED=1 $(CARGO) test

difftest: reference capi pam
	OPENRC_REF_REQUIRED=1 $(CARGO) test -p openrc-applets --tests

clippy:
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) clippy -p openrc-applets -p openrc-pam-openrc --features openrc-applets/pam --all-targets --target-dir target/pam -- -D warnings

fmt:
	$(CARGO) fmt --all

# Applet symlinks come from `openrc-rs --list` (`<dir> <name>` lines), so
# the applet table in crates/applets is the single source of truth.
install: all
	install -d $(DESTDIR)$(RC_LIBEXECDIR)/bin $(DESTDIR)$(RC_LIBEXECDIR)/sbin \
		$(DESTDIR)$(BINDIR) $(DESTDIR)$(SBINDIR) $(DESTDIR)$(LIBDIR) $(DESTDIR)$(INCLUDEDIR)
	install -m 0755 $(BIN) $(DESTDIR)$(RC_LIBEXECDIR)/openrc-rs
	$(BIN) --list | while read -r dir name; do \
		case $$dir in \
			rc-bin)  d=$(RC_LIBEXECDIR)/bin ;; \
			rc-sbin) d=$(RC_LIBEXECDIR)/sbin ;; \
			bin)     d=$(BINDIR) ;; \
			sbin)    d=$(SBINDIR) ;; \
			*) echo "unknown install dir $$dir" >&2; exit 1 ;; \
		esac; \
		ln -sfn $$(realpath -m --relative-to=$$d $(RC_LIBEXECDIR)/openrc-rs) $(DESTDIR)$$d/$$name || exit 1; \
	done
	install -m 0755 $(CAPI_DIR)/libeinfo.so.1 $(DESTDIR)$(LIBDIR)/libeinfo.so.1
	ln -sfn libeinfo.so.1 $(DESTDIR)$(LIBDIR)/libeinfo.so
	install -m 0644 crates/capi-einfo/include/einfo.h $(DESTDIR)$(INCLUDEDIR)/einfo.h
	install -m 0755 $(CAPI_DIR)/librc.so.1 $(DESTDIR)$(LIBDIR)/librc.so.1
	ln -sfn librc.so.1 $(DESTDIR)$(LIBDIR)/librc.so
	install -m 0644 $(RC_DIR)/include/rc.h $(DESTDIR)$(INCLUDEDIR)/rc.h
ifeq ($(PAM),yes)
	install -d $(DESTDIR)$(PAMLIBDIR)
	install -m 0755 $(TARGET_DIR)/libpam_openrc.so $(DESTDIR)$(PAMLIBDIR)/pam_openrc.so
endif

clean:
	$(CARGO) clean

, := ,
FORCE:

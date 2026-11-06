# Makefile - front door for building, testing and installing openrc-rs
#
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261106 - Document OPENRC_BRANDING (distro name in --version) - Claude - scottfk
# 20261106 - Build hybrid librc.so.1 (Rust + weakened upstream C) - Claude - scottfk
#
# Cargo builds everything except the final link of the C ABI libraries
# (the variadic printf-style functions are C, see crates/capi-einfo/), so
# that step and installation live here.
#
#   make              release build: multi-call binary + libeinfo.so.1 + librc.so.1
#   make check        build the C reference if needed, run every test
#                     (unit + differential), failing if the reference is missing
#   make install DESTDIR=/some/stage   install with applet symlinks
#
# Install layout variables match upstream's meson defaults.  Set
# OPENRC_BRANDING="Gentoo Linux" (upstream's -Dbranding) to match a distro
# build's --version output; cargo rebuilds when it changes.

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
DESTDIR    ?=

CARGO_PROFILE_FLAG := $(if $(filter release,$(PROFILE)),--release,)
TARGET_DIR := target/$(if $(filter release,$(PROFILE)),release,debug)
CAPI_DIR   := target/capi
BIN        := $(TARGET_DIR)/openrc-rs

# From `cargo rustc -- --print native-static-libs`.
RUST_NATIVE_LIBS := -lgcc_s -lutil -lrt -lpthread -lm -ldl -lc

# Exports implemented in Rust; -u makes the linker pull them out of the
# static archive (nothing in the C shim references them).
EINFO_RUST_EXPORTS := ebracket ecolor eindent eindentv eoutdent eoutdentv eprefix

.PHONY: all build capi reference test check difftest clippy fmt install clean

all: build capi

build:
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-applets

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

# --- librc.so.1: Rust for the functions in rust-owned.txt, upstream C for
# the rest.  The C objects get those symbols weakened, and the Rust archive
# comes first with -u for each, so its strong definitions are pulled in and
# win, also for calls from inside the C (compiled -fPIC with semantic
# interposition, so those calls go through the PLT).
RC_DIR      := crates/capi-rc
RC_UPSTREAM := $(RC_DIR)/upstream
RC_OBJDIR   := $(CAPI_DIR)/rc-obj
RC_C_SRCS   := librc.c librc-daemon.c librc-depend.c librc-misc.c librc-stringlist.c
RC_OBJS     := $(addprefix $(RC_OBJDIR)/,$(RC_C_SRCS:.c=.o))
RC_RUST_OWNED := $(shell grep -v '^[#]' $(RC_DIR)/rust-owned.txt)
RC_CFLAGS   := -std=c99 -fPIC -fsemantic-interposition -O2 -g \
	-D_FILE_OFFSET_BITS=64 -D_DEFAULT_SOURCE -D_GNU_SOURCE \
	-DHAVE_CLOSE_RANGE -DHAVE_MALLOC_EXTENDED_ATTRIBUTE -DHAVE_STRLCPY \
	'-D_RC_PATH="/etc/rc","/etc","/usr/local/rc","/usr/local","/usr/libexec/rc","/run/openrc"' \
	-I$(RC_UPSTREAM)

$(TARGET_DIR)/libopenrc_capi_rc.a: FORCE
	$(CARGO) build $(CARGO_PROFILE_FLAG) -p openrc-capi-rc

$(RC_OBJDIR)/%.o: $(RC_UPSTREAM)/%.c $(RC_DIR)/rust-owned.txt $(wildcard $(RC_UPSTREAM)/*.h)
	@mkdir -p $(RC_OBJDIR)
	$(CC) $(RC_CFLAGS) -c -o $@.tmp $<
	objcopy $(addprefix --weaken-symbol=,$(RC_RUST_OWNED)) $@.tmp $@
	@rm -f $@.tmp

$(CAPI_DIR)/librc.map: $(RC_DIR)/exports.txt
	@mkdir -p $(CAPI_DIR)
	{ echo '{ global:'; sed 's/$$/;/' $<; echo 'local: *; };'; } > $@

$(CAPI_DIR)/librc.so.1: $(TARGET_DIR)/libopenrc_capi_rc.a $(RC_OBJS) $(CAPI_DIR)/librc.map
	$(CC) $(CFLAGS) -fPIC -shared -o $@ \
		-Wl,-soname,librc.so.1 -Wl,--as-needed \
		-Wl,--version-script=$(CAPI_DIR)/librc.map \
		$(addprefix -Wl$(,)-u$(,),$(RC_RUST_OWNED)) \
		$(TARGET_DIR)/libopenrc_capi_rc.a $(RC_OBJS) $(RUST_NATIVE_LIBS)
	ln -sf librc.so.1 $(CAPI_DIR)/librc.so

capi: $(CAPI_DIR)/libeinfo.so.1 $(CAPI_DIR)/librc.so.1

reference:
	tools/build-reference.sh

test: capi
	$(CARGO) test

check: reference capi
	OPENRC_REF_REQUIRED=1 $(CARGO) test

difftest: reference capi
	OPENRC_REF_REQUIRED=1 $(CARGO) test -p openrc-applets --tests

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

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
	install -m 0644 $(RC_UPSTREAM)/rc.h $(DESTDIR)$(INCLUDEDIR)/rc.h

clean:
	$(CARGO) clean

, := ,
FORCE:

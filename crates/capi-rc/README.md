# capi-rc: the drop-in `librc.so.1`

`make capi` links `target/capi/librc.so.1` from this crate's static
archive (`src/lib.rs` and `src/stringlist.rs`, on top of `crates/rc`).
The library is all Rust.

The library exports exactly the 72 symbols of upstream's `librc.so.1`
([`exports.txt`](exports.txt)), unversioned like upstream. The link passes
`-u` for each one so the linker pulls it out of the archive, and a version
script hides everything else.

Lists are upstream's `queue(3)` TAILQ layout (`RC_STRINGLIST`/`RC_STRING`),
built from `malloc`'d nodes and strings, so C callers can walk, edit and
free them with the usual macros and `free()`.

During Phase 2 the library was a hybrid of Rust and OpenRC 0.64.1's own
`librc` C sources. Each step moved functions to Rust and weakened the C
copies. The C was deleted once nothing was left in it. The differential
tests (`crates/applets/tests/diff_*.rs`) run C OpenRC's programs and
`tests/capi/rc-probe.c` on both the reference library and this one.

[`include/rc.h`](include/rc.h) is the header meson generates at the
reference tag, with the default paths; `make install` installs it, and
rc-probe compiles against it. It is BSD-2-Clause, copyright the OpenRC
authors (see the top-level LICENSE).

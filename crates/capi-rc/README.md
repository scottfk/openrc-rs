# capi-rc: the drop-in `librc.so.1`

`make capi` links `target/capi/librc.so.1` from two halves:

- **Rust** (`src/lib.rs`, on top of `crates/rc`): the functions listed in
  [`rust-owned.txt`](rust-owned.txt).
- **Upstream C** ([`upstream/`](upstream/)): OpenRC 0.64.1's own `librc`
  sources, unmodified, for everything not yet ported.

The C objects are compiled with `-fPIC -fsemantic-interposition`, so calls
between global functions go through the PLT. `objcopy --weaken-symbol`
then makes their definitions of the Rust-owned functions weak. The link puts
the Rust archive first, with `-u` for each Rust-owned symbol, so the Rust
definitions get pulled in and win. That covers calls from inside the
remaining C too, so there is only ever one copy of each piece of state
(svcdir, user-mode paths, the rc.conf cache, cached directory fds).

The library exports exactly the 72 symbols of upstream's `librc.so.1`
([`exports.txt`](exports.txt)), unversioned like upstream.

Each Phase 2 step moves functions from the C half to Rust: implement them in
`crates/rc` and `src/lib.rs`, add them to `rust-owned.txt`, and the
differential tests (`crates/applets/tests/diff_librc.rs`, which run C
OpenRC's programs on both libraries) show whether anything changed. When
everything is in Rust, `upstream/` is deleted.

`upstream/` is BSD-2-Clause, copyright the OpenRC authors (see the
top-level LICENSE). `rc.h` is the header meson generates at the reference
tag, with the default paths; it is also what `make install` installs.

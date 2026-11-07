# Contributing

Thanks for stopping by. This is a personal project, a Rust port of
[OpenRC](https://github.com/OpenRC/openrc) that I tinker with in my spare time, but
contributions are welcome. Here's how things work.

## What this project is

openrc-rs aims to be a **drop-in replacement** for the C OpenRC. Given the same
arguments, environment and filesystem, it should produce the same stdout, stderr, exit
status and side effects, down to the byte. It is not a redesign: if upstream has an odd
edge case, we copy the odd edge case.

The upstream release we track is pinned in [tools/REFERENCE_TAG](tools/REFERENCE_TAG).
Behaviour is checked against a build of that tag, not against the documentation or
against what seems sensible.

Layout:

| Crate | What it is |
|-------|------------|
| `crates/compat` | glibc/C-compatible helpers (getopt, number parsing, stdio, strerror text). **The only crate that calls libc directly.** |
| `crates/einfo` | Port of libeinfo (`einfo`, `ewarn`, `ebegin`/`eend`, ...). |
| `crates/rc` | Port of librc (config, service state, dependency tree). |
| `crates/applets` | The `openrc-rs` multi-call binary. The applet is chosen by `argv[0]`, BusyBox-style. |
| `crates/capi-einfo` | A C ABI `libeinfo.so.1` built from the Rust code, with a small C shim for the variadic functions. |
| `crates/capi-rc` | A C ABI `librc.so.1` built from the Rust code (no C). |

## What's useful to contribute

- **Behaviour differences.** If openrc-rs does something different from C OpenRC at the
  pinned tag, that's a bug. Please send the exact command, environment, and both outputs.
  A new differential test case is even better.
- **New applets / library functions.** Porting another piece of OpenRC (`rc-service`,
  `rc-status`, `start-stop-daemon`, more of librc, ...) is very welcome. Please open an
  issue first so we don't duplicate work, and keep each PR to one applet or subsystem.
- **Reference bumps.** Moving `tools/REFERENCE_TAG` to a newer upstream release, plus the
  fixes needed so the differential tests pass again.
- **Documentation.** Corrections or additions, especially build and install notes for
  different distros and init setups.

## What doesn't fit

- **"Improvements" to upstream behaviour.** Nicer error messages, new flags, changed
  defaults and so on belong upstream in OpenRC first. Once they ship in a tagged release,
  we'll match them.
- **New dependencies** without a strong reason. The workspace depends only on `libc`, and
  I'd like to keep it that way. Things like argument parsing and number formatting are
  reimplemented in `compat` on purpose, because their exact C semantics leak into output.
- **`unsafe` outside `crates/compat`** (and the FFI boundaries in `crates/capi-einfo` and `crates/capi-rc`). If you
  need a new syscall, add a safe wrapper in `compat::sys`.

## Building and testing

You need Rust 1.88 or newer (edition 2024), a C compiler, and for the differential tests,
the tools to build upstream OpenRC (`git`, `meson`, `ninja`).

```sh
make                # release build: target/release/openrc-rs + target/capi/libeinfo.so.1
make test           # cargo test; differential tests are skipped if no reference exists
make reference      # build the C reference at the pinned tag into target/reference/
make check          # build the reference if needed and run everything; fails if it's missing
make clippy         # cargo clippy --all-targets -D warnings
make fmt            # cargo fmt --all
```

`make reference` clones upstream by default. Set `OPENRC_SRC=/path/to/openrc` to export
the tag from a local checkout instead.

**Before sending a PR, please make sure `make check`, `make clippy` and `make fmt` are
clean.** Differential tests that were skipped don't count as passing.

### Differential tests

Most testing is done by running the C reference and openrc-rs side by side on the same
input and checking that stdout, stderr, exit status, and (when a sandbox is used) the
resulting filesystem tree are identical. The harness lives in
[crates/applets/tests/common/mod.rs](crates/applets/tests/common/mod.rs), and each applet
has its own `diff_<applet>.rs` file.

When you fix a behaviour difference or port something new, add cases that cover it,
including error paths and odd arguments. Unit tests are fine for internal helpers, but a
differential test is what shows the behaviour matches.

## Code conventions

- **Port, don't paraphrase.** Each ported module's doc comment should name the upstream
  source file it corresponds to (e.g. `port of src/checkpath/checkpath.c`). Keep the
  structure close enough to the C that someone can read the two side by side.
- **Bytes, not strings.** Paths, arguments and environment values are `Vec<u8>`/`&[u8]`,
  as they are in C. Don't round-trip through `String` or `&str` in places where non-UTF-8
  input could show up.
- **Output goes through `compat::stdio`**, so buffering and flush order match C stdio.
- **Every `unsafe` block gets a `// SAFETY:` comment.** The workspace lints enforce this
  (`undocumented_unsafe_blocks`, `unsafe_op_in_unsafe_fn`).
- **New applets** go in `crates/applets/src/applets/` and get registered in the `APPLETS`
  table in `mod.rs`. That table is the single source of truth for `openrc-rs --list` and
  `make install`, so the symlinks come for free.
- **Script headers.** Shell scripts and the Makefile have a changelog comment at the top,
  one line per change: `YYYYMMDD - <description> - <author>`. Please add a line when you
  change one.

## How to submit changes

The repo lives at <https://codeberg.org/scottfk/openrc-rs>.

1. Fork the repo and create a branch.
2. Make your change, with tests.
3. In the PR description, explain **what** changed and **why**. For a behaviour fix,
   include the command that showed the difference. For a port, name the upstream files
   it covers.
4. If you bumped or tested against a different upstream tag, say which one.

There's no CLA. Contributions are accepted under the project's
[BSD-2-Clause license](LICENSE), the same license as upstream OpenRC.

## Issues

You can open issues for questions, not just bugs. If something about the build or the
reference setup is confusing, ask. I can't promise a fast response, but I do read them.

## Code of conduct

Be straightforward and respectful. That's the whole policy. See
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

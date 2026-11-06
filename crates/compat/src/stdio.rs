//! C stdio emulation for stdout/stderr.
//!
//! The C programs write through stdio: stdout is line-buffered on a terminal
//! and fully buffered otherwise, stderr is unbuffered, and `exit()` flushes.
//! Rust's `std::io::stdout()` is always line-buffered, which would change
//! the interleaving of stdout and stderr when both go to the same file.  So
//! all output in openrc-rs goes through here instead.
//!
//! When openrc-rs code runs inside a C program (the libeinfo/librc C ABI),
//! output must go through *that program's* `FILE *stdout` instead, so it
//! interleaves correctly with the program's own `printf`s; the C ABI crates
//! install a [`Backend`] for that.

use std::sync::{Mutex, OnceLock};

/// A standard stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdin,
    Stdout,
    Stderr,
}

impl Stream {
    pub fn fd(self) -> i32 {
        match self {
            Stream::Stdin => 0,
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        }
    }

    /// `isatty(fileno(stream))`.
    pub fn isatty(self) -> bool {
        crate::sys::isatty(self.fd())
    }
}

/// Alternative output sink: `(stream, bytes)`.  Used by the C ABI crates.
pub type Backend = fn(Stream, &[u8]);

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// Route all output through `backend` from now on.  First caller wins.
pub fn set_backend(backend: Backend) {
    let _ = BACKEND.set(backend);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Line,
    Full(usize),
}

#[derive(Debug)]
struct Buffered {
    mode: Option<Mode>,
    buf: Vec<u8>,
}

static STDOUT: Mutex<Buffered> = Mutex::new(Buffered { mode: None, buf: Vec::new() });

fn stdout_mode() -> Mode {
    if crate::sys::isatty(1) {
        Mode::Line
    } else {
        // glibc sizes the buffer from st_blksize, falling back to BUFSIZ.
        Mode::Full(crate::sys::blksize(1).filter(|&n| n > 0).unwrap_or(8192))
    }
}

/// `fwrite(data, 1, len, stream)`.  Returns `data.len()` like `fprintf`
/// would; write errors are ignored, as the C code ignores them.
pub fn write(stream: Stream, data: &[u8]) -> usize {
    if let Some(backend) = BACKEND.get() {
        backend(stream, data);
        return data.len();
    }
    match stream {
        Stream::Stdin => {}
        Stream::Stderr => crate::sys::write_all(2, data),
        Stream::Stdout => {
            let mut out = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
            let mode = *out.mode.get_or_insert_with(stdout_mode);
            out.buf.extend_from_slice(data);
            let flush = match mode {
                Mode::Line => data.contains(&b'\n'),
                Mode::Full(size) => out.buf.len() >= size,
            };
            if flush {
                crate::sys::write_all(1, &out.buf);
                out.buf.clear();
            }
        }
    }
    data.len()
}

/// Convenience: write to stdout.
pub fn out(data: &[u8]) -> usize {
    write(Stream::Stdout, data)
}

/// Convenience: write to stderr.
pub fn err(data: &[u8]) -> usize {
    write(Stream::Stderr, data)
}

/// `fflush(stdout)`.
pub fn flush() {
    let mut out = STDOUT.lock().unwrap_or_else(|e| e.into_inner());
    if !out.buf.is_empty() {
        crate::sys::write_all(1, &out.buf);
        out.buf.clear();
    }
}

/// `exit(code)`: flush stdout, then exit.
pub fn exit(code: i32) -> ! {
    flush();
    std::process::exit(code)
}

//! C- and glibc-compatible building blocks for openrc-rs.
//!
//! openrc-rs aims to be a byte-for-byte drop-in for the C implementation, so
//! the behavior of a lot of small libc routines (number parsing, getopt,
//! stdio buffering, strerror text) leaks into observable output.  This crate
//! reimplements those routines with the exact semantics OpenRC relies on.
//!
//! It is also the only crate in the workspace that talks to libc directly:
//! everything `unsafe` lives in [`sys`], [`stdio`], [`exec`], [`dl`] and [`caps`] behind
//! safe wrappers.

pub mod bytes;
pub mod caps;
pub mod dl;
pub mod env;
pub mod exec;
pub mod getopt;
pub mod num;
#[cfg(feature = "pam")]
pub mod pam;
pub mod stdio;
pub mod sys;

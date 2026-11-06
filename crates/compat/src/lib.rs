//! C- and glibc-compatible building blocks for openrc-rs.
//!
//! openrc-rs aims to be a byte-for-byte drop-in for the C implementation, so
//! the behaviour of a lot of small libc routines (number parsing, getopt,
//! stdio buffering, strerror text) leaks into observable output.  This crate
//! reimplements those routines with the exact semantics OpenRC relies on.
//!
//! It is also the only crate in the workspace that talks to libc directly:
//! everything `unsafe` lives in [`sys`] and [`stdio`] behind safe wrappers.

pub mod bytes;
pub mod env;
pub mod exec;
pub mod getopt;
pub mod num;
pub mod stdio;
pub mod sys;

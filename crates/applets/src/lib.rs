//! The openrc-rs programs, as a library: the multi-call binary
//! (`main.rs`) dispatches to [`applets`], and the PAM module
//! (`crates/pam-openrc`) shares [`shared`]'s service helpers.

pub mod applets;
pub mod plugin;
pub mod rc_logger;
pub mod schedules;
pub mod shared;

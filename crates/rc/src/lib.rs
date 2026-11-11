//! Port of OpenRC's `librc` (`src/librc/`).
//!
//! Phase 1 covers what the first applets need: `rc.conf` parsing
//! ([`conf`]), user mode and the script search path ([`dirs`]),
//! container/VM detection ([`sys`]), the service state directory
//! ([`svcdir`]), per-service options and environment ([`service`]), and
//! recursive mtime checks ([`mtime`]).  Phase 2 adds runlevels and
//! service state ([`state`]), daemon tracking ([`daemon`]) and the
//! dependency tree ([`deptree`]).

pub mod conf;
pub mod daemon;
pub mod deptree;
pub mod dirs;
pub mod misc;
pub mod mtime;
pub mod paths;
pub mod service;
pub mod state;
pub mod svcdir;
pub mod sys;

pub use conf::rc_conf_value;
pub use dirs::{rc_is_user, rc_set_user};
pub use misc::rc_yesno;
pub use sys::rc_sys;

/// The upstream OpenRC release this port is compatible with.  Reported by
/// `--version`, since scripts may compare against it.
pub const VERSION: &str = "0.64.1";

/// Distribution branding shown by `--version`, e.g. `Gentoo Linux`
/// (upstream's `-Dbranding=`).  Set `OPENRC_BRANDING` at build time.
pub const BRANDING: Option<&str> = option_env!("OPENRC_BRANDING");

/// The program name in the boot banners and `--version`.  `OpenRC`, as
/// upstream prints it, unless `OPENRC_NAME` is set at build time (e.g.
/// `OpenRC-rs`, to show which implementation is installed).  Not an
/// upstream option: the default keeps output identical to C.
pub const NAME: &str = match option_env!("OPENRC_NAME") {
    Some(name) => name,
    None => "OpenRC",
};

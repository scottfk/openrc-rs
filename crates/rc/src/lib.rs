//! Port of OpenRC's `librc` (`src/librc/`).
//!
//! Phase 1 covers what the first applets need: `rc.conf` parsing
//! ([`conf`]), user mode and the script search path ([`dirs`]), and
//! container/VM detection ([`sys`]).  Service state and the dependency tree
//! come next.

pub mod conf;
pub mod dirs;
pub mod misc;
pub mod paths;
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

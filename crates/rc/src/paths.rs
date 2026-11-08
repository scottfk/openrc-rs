//! Compiled-in paths (upstream's `rc.h.in` / meson options).  Override at
//! build time with the environment variables named below.

const fn or(v: Option<&'static str>, default: &'static str) -> &'static str {
    match v {
        Some(v) => v,
        None => default,
    }
}

/// `SYSCONFDIR` (`OPENRC_SYSCONFDIR`, default `/etc`).
pub const SYSCONFDIR: &str = or(option_env!("OPENRC_SYSCONFDIR"), "/etc");
/// `RC_LIBEXECDIR` (`OPENRC_LIBEXECDIR`, default `/usr/libexec/rc`).
pub const LIBEXECDIR: &str = or(option_env!("OPENRC_LIBEXECDIR"), "/usr/libexec/rc");
/// `LOCAL_PREFIX` (`OPENRC_LOCAL_PREFIX`, default `/usr/local`).
pub const LOCAL_PREFIX: &str = or(option_env!("OPENRC_LOCAL_PREFIX"), "/usr/local");
/// `RC_PLUGINDIR` (`OPENRC_PLUGINDIR`, default `/usr/lib64/rc/plugins`:
/// upstream's `$libdir/rc/plugins` with Gentoo amd64's libdir).
pub const PLUGINDIR: &str = or(option_env!("OPENRC_PLUGINDIR"), "/usr/lib64/rc/plugins");
/// `RC_SVCDIR`.
pub const SVCDIR: &str = "/run/openrc";

/// `RC_CONF_OLD`: `$SYSCONFDIR/conf.d/rc`.
pub fn rc_conf_old() -> Vec<u8> {
    format!("{SYSCONFDIR}/conf.d/rc").into_bytes()
}

/// The default script search path (`_RC_PATH`), highest priority first.
pub fn default_rc_path() -> Vec<Vec<u8>> {
    [
        format!("{SYSCONFDIR}/rc"),
        SYSCONFDIR.to_string(),
        format!("{LOCAL_PREFIX}/rc"),
        LOCAL_PREFIX.to_string(),
        LIBEXECDIR.to_string(),
        // Upstream appends this literal, not RC_SVCDIR.
        "/run/openrc".to_string(),
    ]
    .into_iter()
    .map(String::into_bytes)
    .collect()
}

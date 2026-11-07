//! Port of upstream's `src/shared/` helpers used by several programs:
//! the common options and usage text (`_usage.c`), `parse_mode`, and the
//! `misc.c` helpers `default_runlevel()`, `_rc_deptree_load()`,
//! `env_filter()` and `env_config()`.

use std::sync::atomic::{AtomicU32, Ordering};

use openrc_compat::bcat;
use openrc_compat::env;
use openrc_compat::getopt::{HasArg, LongOpt};
use openrc_compat::num::strtoul;
use openrc_compat::stdio;

/// `getoptstring_COMMON`, for `concat!`.
macro_rules! getoptstring_common {
    () => {
        "ChqVvU"
    };
}
pub(crate) use getoptstring_common;

/// `longopts_COMMON`.
pub const LONGOPTS_COMMON: &[LongOpt] = &[
    LongOpt::new("help", HasArg::No, b'h'),
    LongOpt::new("nocolor", HasArg::No, b'C'),
    LongOpt::new("version", HasArg::No, b'V'),
    LongOpt::new("verbose", HasArg::No, b'v'),
    LongOpt::new("quiet", HasArg::No, b'q'),
    LongOpt::new("user", HasArg::No, b'U'),
];

/// `longopts_help_COMMON`.
pub const LONGOPTS_HELP_COMMON: &[&str] = &[
    "Display this help output",
    "Disable color output",
    "Display software version",
    "Run verbosely",
    "Run quietly (repeat to suppress errors)",
    "Run in user mode",
];

/// What upstream keeps in per-program globals for `usage()`.
#[derive(Debug)]
pub struct Usage {
    pub applet: Vec<u8>,
    pub getoptstring: &'static str,
    /// Program-specific options; the common ones are appended.
    pub longopts: &'static [LongOpt],
    pub longopts_help: &'static [&'static str],
    pub extraopts: Option<&'static str>,
    pub usagestring: Option<&'static str>,
}

impl Usage {
    /// All long options, program-specific first.
    pub fn all_longopts(&self) -> Vec<LongOpt> {
        self.longopts.iter().chain(LONGOPTS_COMMON).copied().collect()
    }

    /// `usage(exit_status)`.
    pub fn usage(&self, exit_status: i32) -> ! {
        const HAS_ARG: [&str; 3] = ["", "<arg>", "[arg]"];
        let mut out = match self.usagestring {
            Some(u) => u.as_bytes().to_vec(),
            None => bcat!("Usage: ", self.applet, " [options] "),
        };
        if let Some(e) = self.extraopts {
            out.extend_from_slice(e.as_bytes());
        }
        out.extend_from_slice(format!("\n\nOptions: [ {} ]\n", self.getoptstring).as_bytes());

        let help = self.longopts_help.iter().chain(LONGOPTS_HELP_COMMON);
        for (opt, help) in self.all_longopts().iter().zip(help) {
            // isprint(val) in the C locale, on the whole int: long-only
            // options (seedrng's 0x101...) get no short form.
            let short = match opt.val {
                c @ 0x20..=0x7e => format!("-{},", c as u8 as char),
                _ => String::new(),
            };
            let has_arg = HAS_ARG[match opt.has_arg {
                HasArg::No => 0,
                HasArg::Required => 1,
                HasArg::Optional => 2,
            }];
            let head = format!("  {short:>3} --{} {has_arg}", opt.name);
            let mut len = head.len() as i32;
            out.extend_from_slice(head.as_bytes());
            for token in help.split('\n') {
                let pad = 36 - len;
                if pad > 0 {
                    out.extend(std::iter::repeat_n(b' ', pad as usize));
                }
                out.extend_from_slice(token.as_bytes());
                out.push(b'\n');
                len = 0;
            }
        }
        stdio::out(&out);
        stdio::exit(exit_status)
    }

    /// Handle one of the common options (`case_RC_COMMON_GETOPT`);
    /// anything else is a usage error.  `argc` is the original argc.
    pub fn common_option(&self, opt: i32, argc: usize) {
        match u8::try_from(opt).unwrap_or(0) {
            b'C' => env::set("EINFO_COLOR", b"NO"),
            b'h' => self.usage(0),
            b'V' => {
                if argc == 2 {
                    show_version(&self.applet);
                }
            }
            b'v' => env::set("EINFO_VERBOSE", b"YES"),
            b'q' => set_quiet_options(),
            b'U' => rc::rc_set_user(),
            _ => self.usage(1),
        }
    }
}

/// `set_quiet_options()`: the first -q quiets einfo, the second eerror too.
pub fn set_quiet_options() {
    static QCOUNT: AtomicU32 = AtomicU32::new(0);
    match QCOUNT.fetch_add(1, Ordering::Relaxed) + 1 {
        1 => env::set("EINFO_QUIET", b"YES"),
        2 => env::set("EERROR_QUIET", b"YES"),
        _ => {}
    }
}

/// `show_version()`: `applet (OpenRC [sys]) version (branding)`.
pub fn show_version(applet: &[u8]) -> ! {
    let mut out = bcat!(applet, " (OpenRC");
    if let Some(sys) = rc::rc_sys() {
        out.extend_from_slice(format!(" [{sys}]").as_bytes());
    }
    out.extend_from_slice(format!(") {}", rc::VERSION).as_bytes());
    if let Some(b) = rc::BRANDING {
        out.extend_from_slice(format!(" ({b})").as_bytes());
    }
    out.push(b'\n');
    stdio::out(&out);
    stdio::exit(0)
}

/// `default_runlevel()`: the current runlevel, else `rc_default_runlevel`
/// from rc.conf, else `default`.
pub fn default_runlevel() -> Vec<u8> {
    let runlevel = rc::state::rc_runlevel_get().or_else(|| rc::conf::rc_conf_value("rc_default_runlevel"));
    match runlevel {
        Some(r) if !r.is_empty() => r,
        _ => b"default".to_vec(),
    }
}

/// `_rc_deptree_load(force, regen)`: regenerate the dependency cache if
/// it is stale (or `force` is set) and we may write it, then load it.
/// `regen` is set to 1 when it was regenerated (with `force == -1`, to the
/// update's result: 0 or -1).
pub fn rc_deptree_load_updating(force: i32, regen: Option<&mut i32>) -> Option<rc::deptree::DepTree> {
    use openrc_compat::sys::{self, at, libc};
    use openrc_einfo::{ebegin, eend, eerror, eerrorn};
    use rc::svcdir::{RcDir, rc_dirfd_raw, rc_svcdir};

    let un = rc::deptree::rc_deptree_update_needed();
    if un.needed || force != 0 {
        // C's `t` is only written when an update is needed.
        let t = if un.needed { un.newest } else { 0 };
        let svcdirfd = rc_dirfd_raw(RcDir::Svcdir);

        // Test if we have permission to update the deptree.
        if let Err(e) = at::openat(svcdirfd, b"deptree", libc::O_WRONLY, 0)
            && e.0 == libc::EACCES
        {
            return rc::deptree::rc_deptree_load();
        }

        let mut regen = regen;
        if let Some(r) = regen.as_deref_mut() {
            *r = 1;
        }
        ebegin(Some(b"Caching service dependencies"));
        let retval = if rc::deptree::rc_deptree_update() { 0 } else { -1 };
        eend(retval, Some(b"Failed to update the dependency tree"));

        if retval == 0 {
            let st = match at::fstatat(svcdirfd, b"deptree", 0) {
                Ok(st) => st,
                Err(e) => {
                    // QUIRK: the format puts the path after the error.
                    eerror(Some(&bcat!("stat(", rc_svcdir(), "): ", e.strerror(), "/deptree")));
                    return None;
                }
            };
            if st.mtime >= t {
                let _ = at::unlinkat(svcdirfd, b"clock-skewed", 0);
            } else {
                let file = un.file.unwrap_or_default();
                eerror(Some(&bcat!("Clock skew detected with '", file, "'")));
                eerrorn(Some(&bcat!("Adjusting mtime of '", rc_svcdir(), "/deptree' to ", sys::ctime(t))));
                let _ = at::utimensat(svcdirfd, b"deptree", t, 0);
                if let Ok(fd) =
                    at::openat(svcdirfd, b"clock-skewed", libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC, 0o666)
                {
                    use std::io::Write;
                    let _ = std::fs::File::from(fd).write_all(&bcat!(file, "\n"));
                }
            }
        }
        if force == -1
            && let Some(r) = regen
        {
            *r = retval;
        }
    }
    rc::deptree::rc_deptree_load()
}

/// `lookup_service_state()` (shared/misc.c): the state bit for
/// `service_started` & co.  There is no `service_scheduled`.
pub fn lookup_service_state(name: &[u8]) -> i32 {
    use rc::state::flags as st;
    match name {
        b"service_started" => st::STARTED,
        b"service_stopped" => st::STOPPED,
        b"service_inactive" => st::INACTIVE,
        b"service_starting" => st::STARTING,
        b"service_stopping" => st::STOPPING,
        b"service_hotplugged" => st::HOTPLUGGED,
        b"service_wasinactive" => st::WASINACTIVE,
        b"service_failed" => st::FAILED,
        b"service_crashed" => st::CRASHED,
        _ => 0,
    }
}

/// `rc_conf_yesno()`: [`rc::misc::rc_yesno_errno`] on an rc.conf setting.
pub fn rc_conf_yesno(setting: &str) -> (bool, Option<i32>) {
    rc::misc::rc_yesno_errno(rc::conf::rc_conf_value(setting).as_deref())
}

/// `env_allowlist` (shared/misc.c).
const ENV_ALLOWLIST: &[&str] = &[
    "EERROR_QUIET",
    "EINFO_QUIET",
    "IN_BACKGROUND",
    "IN_DRYRUN",
    "IN_HOTPLUG",
    "RC_DEBUG",
    "RC_NODEPS",
    "RC_USER_SERVICES",
    "RC_SVCDIR",
    "RC_PATH",
    "LANG",
    "LC_MESSAGES",
    "TERM",
    "EINFO_COLOR",
    "EINFO_VERBOSE",
    "HOSTNAME",
    "CHARSET",
    "LC_*",
];

/// `usrenv_allowlist`: also kept in user mode.
const USRENV_ALLOWLIST: &[&str] =
    &["USER", "LOGNAME", "HOME", "SHELL", "XDG_*", "DIFFTOOL", "LESS", "PAGER", "EDITOR", "VISUAL"];

fn env_match(pattern: &[u8], name: &[u8]) -> bool {
    use openrc_compat::sys::{self, libc};
    sys::fnmatch(pattern, name, libc::FNM_NOESCAPE | libc::FNM_PERIOD)
}

/// `env_filter()`: drop every environment variable that isn't on the
/// allow lists or in `rc_env_allow`, then add what's missing from
/// `profile.env`.  `rc_env_allow="*"` keeps everything (and skips
/// profile.env).
pub fn env_filter() {
    use openrc_compat::bytes::as_path;

    // rc_stringlist_split(value, " "): empty pieces kept; NULL gives none.
    let env_allow: Vec<Vec<u8>> = rc::conf::rc_conf_value("rc_env_allow")
        .map(|v| v.split(|&b| b == b' ').map(<[u8]>::to_vec).collect())
        .unwrap_or_default();
    if env_allow.iter().any(|a| a == b"*") {
        return;
    }

    let mut profile =
        rc::conf::rc_config_load(as_path(format!("{}/profile.env", rc::dirs::rc_sysconfdir()).as_bytes()));
    if rc::dirs::rc_is_user() {
        // xasprintf("%s/profile.env", NULL) would give "(null)/profile.env".
        let dir = rc::dirs::rc_usrconfdir().unwrap_or_else(|| b"(null)".to_vec());
        profile.extend(rc::conf::rc_config_load(as_path(&bcat!(dir, "/profile.env"))));
    }

    // Work from a copy of the names.
    let names: Vec<Vec<u8>> = env::environ()
        .into_iter()
        .map(|e| match e.iter().position(|&b| b == b'=') {
            Some(i) => e[..i].to_vec(),
            None => e,
        })
        .collect();
    for name in names {
        if ENV_ALLOWLIST.iter().any(|p| env_match(p.as_bytes(), &name)) {
            continue;
        }
        if rc::dirs::rc_is_user() && USRENV_ALLOWLIST.iter().any(|p| env_match(p.as_bytes(), &name)) {
            continue;
        }
        if env_allow.iter().any(|p| env_match(p, &name)) {
            continue;
        }
        env::unset_bytes(&name);
    }

    // Now add anything missing from the profile.
    for entry in profile {
        let eq = entry.iter().position(|&b| b == b'=').unwrap_or(entry.len());
        let (name, value) = (&entry[..eq], entry.get(eq + 1..).unwrap_or_default());
        if env::get_bytes(name).is_none() {
            env::set_bytes(name, value);
        }
    }
}

/// `RC_PATH_PREFIX`.
fn rc_path_prefix() -> Vec<u8> {
    bcat!(rc::paths::LIBEXECDIR, "/bin:/bin:/sbin:/usr/bin:/usr/sbin")
}

/// `env_config()`: the environment init scripts expect (`RC_*`, a PATH
/// starting with the system directories, einfo settings from rc.conf).
pub fn env_config() {
    use openrc_compat::sys;

    let prefix = rc_path_prefix();
    match env::get("PATH") {
        None => env::set("PATH", &prefix),
        Some(path) if !path.starts_with(&prefix) => {
            // Keep only the parts of PATH that aren't already in the prefix.
            let mut np = prefix.clone();
            for token in path.split(|&b| b == b':') {
                if !prefix.split(|&b| b == b':').any(|t| t == token) {
                    np.push(b':');
                    np.extend_from_slice(token);
                }
            }
            env::unset("PATH");
            env::set("PATH", &np);
        }
        Some(_) => {}
    }

    if !rc::dirs::rc_is_user() {
        env::set("RC_CACHEDIR", b"/var/cache/rc");
    } else if let Some(c) = env::get("XDG_CACHE_HOME") {
        env::set("RC_CACHEDIR", &bcat!(c, "/rc"));
    } else if let Some(h) = env::get("HOME") {
        env::set("RC_CACHEDIR", &bcat!(h, "/.cache/rc"));
    }

    let svcdir = rc::svcdir::rc_svcdir();
    let runlevel = default_runlevel();
    env::set("RC_VERSION", rc::VERSION.as_bytes());
    env::set("RC_LIBEXECDIR", rc::paths::LIBEXECDIR.as_bytes());
    env::set("RC_SVCDIR", &svcdir);
    env::set("RC_TMPDIR", &bcat!(svcdir, "/tmp"));
    env::set("RC_BOOTLEVEL", b"boot");
    env::set("RC_RUNLEVEL", &runlevel);
    env::set("RC_PATH", &rc::dirs::rc_scriptdirs().join(&b':'));

    // krunlevel is a system-wide override; it never applies to user
    // sessions.  (A fixed path: RC_SVCDIR "/krunlevel".)
    let krunlevel = if rc::dirs::rc_is_user() {
        None
    } else {
        std::fs::File::open(format!("{}/krunlevel", rc::paths::SVCDIR)).ok()
    };
    let deflevel = krunlevel.and_then(|f| {
        use std::io::BufRead;
        // xgetline(): one line without its '\n'; nothing on EOF or error.
        let mut line = Vec::new();
        match std::io::BufReader::new(f).read_until(b'\n', &mut line) {
            Ok(n) if n > 0 => {
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                Some(openrc_compat::bytes::c_str_prefix(&line).to_vec())
            }
            _ => None,
        }
    });
    env::set("RC_DEFAULTLEVEL", &deflevel.unwrap_or_else(default_runlevel));

    if let Some(sys) = rc::rc_sys() {
        env::set("RC_SYS", sys.as_bytes());
    }
    if let Some(u) = sys::uname_sysname() {
        env::set("RC_UNAME", &u);
    }

    // Be quiet or verbose as necessary.
    if rc_conf_yesno("rc_quiet").0 {
        env::set("EINFO_QUIET", b"YES");
    }
    if rc_conf_yesno("rc_verbose").0 {
        env::set("EINFO_VERBOSE", b"YES");
    }
    // `!yesno && errno == 0`: only an explicit "no" turns colour off.
    let (color, err) = rc_conf_yesno("rc_color");
    if (!color && err.is_none()) || rc_conf_yesno("rc_nocolor").0 {
        env::set("EINFO_COLOR", b"NO");
    }
}

/// `parse_mode()`: only numeric (octal) modes are supported.
pub fn parse_mode(text: &[u8]) -> Option<u32> {
    // QUIRK: `(*text - '0') < 8` is signed, so anything sorting below '0'
    // (including "", "+", "-", " ") also takes the numeric path; e.g. an
    // empty mode is accepted as 0 ("don't check").
    let first = text.first().map_or(0, |&b| i32::from(b as i8));
    if first - i32::from(b'0') < 8 {
        let p = strtoul(text, 8);
        if !p.complete(text) || p.value > 0o7777 {
            return None;
        }
        return Some(p.value as u32);
    }
    None
}

/// `parse_duration()` (`shared/timeutils.c`): "20", "500ms", "2min", ...
/// in milliseconds; `None` if invalid.  (Upstream spells "miliseconds".)
pub fn parse_duration(duration: &[u8]) -> Option<i64> {
    const EXTS: &[(&str, i64)] = &[
        ("ms", 1),
        ("miliseconds", 1),
        ("s", 1000),
        ("sec", 1000),
        ("seconds", 1000),
        ("", 1000),
        ("m", 60_000),
        ("min", 60_000),
        ("minutes", 60_000),
        ("h", 3_600_000),
        ("hour", 3_600_000),
    ];
    let p = openrc_compat::num::strtoimax(duration, 10);
    if p.erange || duration.is_empty() || p.value < 0 {
        return None;
    }
    // strtol leaves endptr at the start when nothing was converted.
    let rest = &duration[p.end..];
    let unit = EXTS.iter().find(|(e, _)| e.as_bytes() == rest)?.1;
    p.value.checked_mul(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes() {
        assert_eq!(parse_mode(b"0755"), Some(0o755));
        assert_eq!(parse_mode(b"644"), Some(0o644));
        assert_eq!(parse_mode(b""), Some(0));
        assert_eq!(parse_mode(b"+7"), Some(7));
        assert_eq!(parse_mode(b"-1"), None);
        assert_eq!(parse_mode(b"8"), None);
        assert_eq!(parse_mode(b"u+x"), None);
        assert_eq!(parse_mode(b"17777"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration(b"20"), Some(20_000));
        assert_eq!(parse_duration(b"500ms"), Some(500));
        assert_eq!(parse_duration(b"2min"), Some(120_000));
        assert_eq!(parse_duration(b"1h"), Some(3_600_000));
        assert_eq!(parse_duration(b""), None);
        assert_eq!(parse_duration(b"-1"), None);
        assert_eq!(parse_duration(b"5 s"), None);
        assert_eq!(parse_duration(b"abc"), None);
    }
}

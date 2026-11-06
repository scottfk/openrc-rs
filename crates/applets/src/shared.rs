//! Port of upstream's `src/shared/` helpers used by several programs:
//! the common options and usage text (`_usage.c`), and `parse_mode`.

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
            let short = match u8::try_from(opt.val & 0xff) {
                // isprint() in the C locale
                Ok(c @ 0x20..=0x7e) => format!("-{},", c as char),
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
}

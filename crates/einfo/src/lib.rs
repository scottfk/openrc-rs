//! Port of OpenRC's `libeinfo` (`src/libeinfo/libeinfo.c`).
//!
//! The einfo family prints status lines in the familiar Gentoo style:
//!
//! ```text
//!  * Starting sshd ...                                      [ ok ]
//! ```
//!
//! Messages are already-formatted byte strings: where C takes a printf
//! format, Rust callers format first, and the C ABI shim (`capi-einfo`) runs
//! `vsnprintf` before calling in.  `None` stands for a NULL format, which
//! most functions treat as "print nothing".
//!
//! Behavior, including upstream quirks, follows 0.64.1 exactly; quirks are
//! marked `QUIRK` and listed in `QUIRKS.md`.  Return values are the byte
//! counts the C functions return.

use std::ffi::CStr;
use std::sync::Mutex;

use openrc_compat::bcat;
use openrc_compat::bytes::find;
use openrc_compat::env;
use openrc_compat::num::{atoi, strtoimax};
use openrc_compat::stdio::{self, Stream};
use openrc_compat::sys;

/// Color types (`ECOLOR`), with the C enum's values.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EColor {
    Normal = 1,
    Good = 2,
    Warn = 3,
    Bad = 4,
    Hilite = 5,
    Bracket = 6,
}

impl EColor {
    /// From the C enum value; unknown values behave like C (no color found).
    pub fn from_raw(v: i32) -> Option<EColor> {
        Some(match v {
            1 => EColor::Normal,
            2 => EColor::Good,
            3 => EColor::Warn,
            4 => EColor::Bad,
            5 => EColor::Hilite,
            6 => EColor::Bracket,
            _ => return None,
        })
    }
}

/// syslog priorities used by the einfo functions.
pub const LOG_ERR: i32 = 3;
pub const LOG_WARNING: i32 = 4;

const DEFAULT_COLS: i32 = 80;
const OK: &[u8] = b"ok";
const NOT_OK: &[u8] = b"!!";
const INDENT_WIDTH: i32 = 2;
const INDENT_MAX: i32 = 40;

// ANSI escape codes which mimic termcap.
const AF: &str = "\x1b[3"; // "\033[3%dm"
const CE: &CStr = c"\x1b[K";
const MD: &str = "\x1b[1m";
const ME: &str = "\x1b[m";
const UP: &CStr = c"\x1b[A";

struct ColorDef {
    color: EColor,
    def: i32,
    name: Option<&'static str>,
}

/// Same order as the C table; `_ecolor` searches it linearly.
const ECOLORS: [ColorDef; 6] = [
    ColorDef { color: EColor::Good, def: 2, name: Some("good") },
    ColorDef { color: EColor::Warn, def: 3, name: Some("warn") },
    ColorDef { color: EColor::Bad, def: 1, name: Some("bad") },
    ColorDef { color: EColor::Hilite, def: 6, name: Some("hilite") },
    ColorDef { color: EColor::Bracket, def: 4, name: Some("bracket") },
    ColorDef { color: EColor::Normal, def: 0, name: None },
];

/// Terminals known to support color; any `$TERM` containing "color" does too.
const COLOR_TERMS: &[&str] = &[
    "Eterm",
    "alacritty",
    "ansi",
    "con132x25",
    "con132x30",
    "con132x43",
    "con132x60",
    "con80x25",
    "con80x28",
    "con80x30",
    "con80x43",
    "con80x50",
    "con80x60",
    "cons25",
    "console",
    "cygwin",
    "dtterm",
    "foot",
    "gnome",
    "konsole",
    "kterm",
    "linux",
    "linux-c",
    "mlterm",
    "putty",
    "rxvt",
    "rxvt-cygwin",
    "rxvt-cygwin-native",
    "rxvt-unicode",
    "screen",
    "screen-bce",
    "screen-w",
    "screen.linux",
    "vt100",
    "vt220",
    "wsvt25",
    "xterm",
    "xterm-debian",
    "xterm-kitty",
    "xterm-ghostty",
];

/// Escape sequences, computed once a color terminal is detected.  Leaked
/// so the C ABI can hand out `const char *` that live forever, as in C.
#[derive(Clone, Copy)]
struct Caps {
    colors: [&'static CStr; 6],
    flush: &'static CStr,
    up: &'static CStr,
}

struct State {
    prefix: Option<Vec<u8>>,
    /// -1 unknown, 0 no, 1 yes (the C `static int in_colour`).
    in_colour: i8,
    /// Cached `$TERM` (the C `static const char *term`).
    term: Option<Vec<u8>>,
    term_is_cons25: bool,
    caps: Option<Caps>,
}

static STATE: Mutex<State> =
    Mutex::new(State { prefix: None, in_colour: -1, term: None, term_is_cons25: false, caps: None });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn leak(bytes: Vec<u8>) -> &'static CStr {
    let c = std::ffi::CString::new(bytes).expect("escape sequences contain no NUL");
    Box::leak(c.into_boxed_c_str())
}

/// libeinfo's private `yesno()`: unlike librc's, it also accepts on/off.
/// Returns (value, recognized).  Like C it sets errno: ENOENT for an unset
/// value, EINVAL for one that's neither yes nor no.  Callers can see this:
/// start-stop-daemon tests errno after an `ewarnv()`, for one.
fn yesno(value: Option<&[u8]>) -> (bool, bool) {
    use openrc_compat::sys::{libc, set_errno};
    let Some(v) = value else {
        set_errno(libc::ENOENT);
        return (false, false);
    };
    let is = |w: &str| v.eq_ignore_ascii_case(w.as_bytes());
    if ["yes", "y", "true", "on", "1"].iter().any(|w| is(w)) {
        return (true, true);
    }
    let recognized = ["no", "n", "false", "off", "0"].iter().any(|w| is(w));
    if !recognized {
        set_errno(libc::EINVAL);
    }
    (false, recognized)
}

/// `noyes()`: true only for an explicit no/n/false/off/0.  errno is kept
/// for those, and left as `yesno()` set it otherwise.
fn noyes(value: Option<&[u8]>) -> bool {
    use openrc_compat::sys::{Errno, set_errno};
    let saved = Errno::last().0;
    set_errno(0);
    match yesno(value) {
        (v, true) => {
            set_errno(saved);
            !v
        }
        (v, false) => v,
    }
}

fn env_yes(name: &str) -> bool {
    yesno(env::get(name).as_deref()).0
}

fn is_quiet() -> bool {
    env_yes("EINFO_QUIET")
}

fn is_really_quiet() -> bool {
    env_yes("EERROR_QUIET")
}

fn is_verbose() -> bool {
    env_yes("EINFO_VERBOSE")
}

fn build_caps() -> Caps {
    let user = env::get("EINFO_COLOR");
    let mut colors = [c""; 6];
    for (i, def) in ECOLORS.iter().enumerate() {
        let tmp = match def.name {
            Some(name) => {
                let mut bold = MD;
                let mut c = def.def;
                // User overrides look like "good=2;1:bad=1;0:" (color;bold).
                if let Some(e) = &user
                    && let Some(at) = find(e, name.as_bytes())
                {
                    let after = &e[at + name.len()..];
                    if after.first() == Some(&b'=') {
                        let mut d = &after[1..];
                        if let Some(colon) = d.iter().position(|&b| b == b':') {
                            d = &d[..colon];
                        }
                        c = atoi(d);
                        if let Some(semi) = d.iter().position(|&b| b == b';')
                            && d.get(semi + 1) == Some(&b'0')
                        {
                            bold = ME;
                        }
                    }
                }
                format!("{bold}{AF}{}m", c & 0x07).into_bytes()
            }
            None => ME.as_bytes().to_vec(),
        };
        colors[i] = leak(tmp);
    }
    Caps { colors, flush: CE, up: UP }
}

/// `colour_terminal(f)`; `None` is a NULL `FILE *` (skips the tty check).
fn colour_terminal(f: Option<Stream>) -> bool {
    if let Some(f) = f
        && !f.isatty()
    {
        return false;
    }
    if noyes(env::get("EINFO_COLOR").as_deref()) {
        return false;
    }

    let mut st = state();
    match st.in_colour {
        0 => return false,
        1 => return true,
        _ => {}
    }

    st.term_is_cons25 = false;
    if st.term.is_none() {
        match env::get("TERM") {
            Some(t) => st.term = Some(t),
            None => return false,
        }
    }
    let term = st.term.clone().expect("set above");
    if term == b"cons25" {
        st.term_is_cons25 = true;
    }
    if find(&term, b"color").is_some() || COLOR_TERMS.iter().any(|t| t.as_bytes() == term) {
        st.in_colour = 1;
    } else {
        st.in_colour = 0;
        return false;
    }

    st.caps = Some(build_caps());
    true
}

/// `get_term_columns(stream)`.
fn get_term_columns(stream: Stream) -> i32 {
    if let Some(cols) = env::get("COLUMNS") {
        let p = strtoimax(&cols, 10);
        if p.complete(&cols) {
            return p.value as i32;
        }
    }
    match sys::term_cols(stream.fd()) {
        Some(c) => i32::from(c),
        None => DEFAULT_COLS,
    }
}

/// `eprefix()`: prefix each einfo line with `prefix|`.
pub fn eprefix(prefix: Option<&[u8]>) {
    state().prefix = prefix.map(<[u8]>::to_vec);
}

/// `elog()`: send `msg` to syslog if `EINFO_LOG` names a syslog ident.
pub fn elog(level: i32, msg: Option<&[u8]>) {
    if let (Some(msg), Some(ident)) = (msg, env::get("EINFO_LOG")) {
        sys::syslog_once(&ident, sys::libc::LOG_PID, sys::libc::LOG_DAEMON, level, msg);
    }
}

fn write(f: Stream, b: &[u8]) -> i32 {
    stdio::write(f, b) as i32
}

/// `_eindent()`: print `$EINFO_INDENT` spaces (clamped to 0..=40).
fn eindent_out(f: Stream) -> i32 {
    let mut amount = 0;
    if let Some(e) = env::get("EINFO_INDENT") {
        // QUIRK: base 0, so the zero-padded "%08d" values written by
        // eindent() are parsed as octal ("00000008" reads as 0).
        let p = strtoimax(&e, 0);
        amount = p.value as i32;
        if p.erange || amount < 0 {
            amount = 0;
        } else if amount > INDENT_MAX {
            amount = INDENT_MAX;
        }
    }
    write(f, &vec![b' '; amount as usize])
}

fn color_for(color: EColor) -> &'static CStr {
    let st = state();
    let Some(caps) = st.caps else { return c"" };
    ECOLORS.iter().position(|d| d.color == color).map_or(c"", |i| caps.colors[i])
}

/// `_ecolor(f, color)`.
fn ecolor_for(f: Option<Stream>, color: EColor) -> &'static CStr {
    if !colour_terminal(f) {
        return c"";
    }
    color_for(color)
}

/// `ecolor()`: the escape sequence for `color`, or "" if not on a color
/// terminal.  Guesses which stream is the tty (stdout, stderr, stdin).
pub fn ecolor(color: EColor) -> &'static CStr {
    let f = [Stream::Stdout, Stream::Stderr, Stream::Stdin].into_iter().find(|s| s.isatty());
    ecolor_for(f, color)
}

/// `ecolor()` for a raw C enum value.
pub fn ecolor_raw(color: i32) -> &'static CStr {
    match EColor::from_raw(color) {
        Some(c) => ecolor(c),
        None => {
            // C still runs the terminal detection before failing the lookup.
            let _ = ecolor(EColor::Normal);
            c""
        }
    }
}

fn lastcmd(cmd: &str) {
    env::unset("EINFO_LASTCMD");
    env::set("EINFO_LASTCMD", cmd.as_bytes());
}

/// `_einfo()`: the common " * msg" line body.
fn einfo_body(f: Stream, color: EColor, msg: &[u8]) -> i32 {
    let mut retval = 0;
    if let Some(last) = env::get("EINFO_LASTCMD")
        && !last.is_empty()
        && !colour_terminal(Some(f))
        && last != b"ewarn"
        && last.last() == Some(&b'n')
    {
        write(f, b"\n");
    }
    let prefix = state().prefix.clone();
    if let Some(prefix) = prefix {
        let line = bcat!(ecolor_for(Some(f), color), prefix, ecolor_for(Some(f), EColor::Normal), "|");
        write(f, &line);
    }
    write(f, &bcat!(" ", ecolor_for(Some(f), color), "*", ecolor_for(Some(f), EColor::Normal), " "));
    retval += eindent_out(f);
    retval += write(f, msg) + 3;
    if colour_terminal(Some(f)) {
        let flush = state().caps.map_or(c"", |c| c.flush);
        write(f, flush.to_bytes());
    }
    retval
}

fn einfovn_(msg: &[u8]) -> i32 {
    einfo_body(Stream::Stdout, EColor::Good, msg)
}
fn ewarnvn_(msg: &[u8]) -> i32 {
    einfo_body(Stream::Stderr, EColor::Warn, msg)
}
fn eerrorvn_(msg: &[u8]) -> i32 {
    einfo_body(Stream::Stderr, EColor::Bad, msg)
}

/// `einfon()`: informational message, no newline.
pub fn einfon(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_quiet()) else { return 0 };
    let r = einfovn_(msg);
    lastcmd("einfon");
    r
}

/// `ewarnn()`: warning, no newline.
pub fn ewarnn(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_quiet()) else { return 0 };
    let r = ewarnvn_(msg);
    lastcmd("ewarnn");
    r
}

/// `eerrorn()`: error, no newline.
pub fn eerrorn(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_really_quiet()) else { return 0 };
    let r = eerrorvn_(msg);
    // QUIRK: upstream records "errorn", not "eerrorn".
    lastcmd("errorn");
    r
}

/// `einfo()`: informational message.
pub fn einfo(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_quiet()) else { return 0 };
    let mut r = einfovn_(msg);
    r += write(Stream::Stdout, b"\n");
    lastcmd("einfo");
    r
}

/// `ewarn()`: warning (also logged if `EINFO_LOG` is set).
pub fn ewarn(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_quiet()) else { return 0 };
    elog(LOG_WARNING, Some(msg));
    let mut r = ewarnvn_(msg);
    r += write(Stream::Stderr, b"\n");
    lastcmd("ewarn");
    r
}

/// `ewarnx()`: warn and `exit(EXIT_FAILURE)`.
pub fn ewarnx(msg: Option<&[u8]>) -> ! {
    if let Some(msg) = msg.filter(|_| !is_quiet()) {
        elog(LOG_WARNING, Some(msg));
        ewarnvn_(msg);
        write(Stream::Stderr, b"\n");
    }
    stdio::exit(1)
}

/// `eerror()`: error (also logged if `EINFO_LOG` is set).
pub fn eerror(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_really_quiet()) else { return 0 };
    elog(LOG_ERR, Some(msg));
    let mut r = eerrorvn_(msg);
    r += write(Stream::Stderr, b"\n");
    lastcmd("eerror");
    r
}

/// `eerrorx()`: error and `exit(EXIT_FAILURE)`.
pub fn eerrorx(msg: Option<&[u8]>) -> ! {
    if let Some(msg) = msg.filter(|_| !is_really_quiet()) {
        elog(LOG_ERR, Some(msg));
        eerrorvn_(msg);
        write(Stream::Stderr, b"\n");
    }
    stdio::exit(1)
}

fn ebegin_tail(r: &mut i32) {
    *r += write(Stream::Stdout, b" ...");
    if colour_terminal(Some(Stream::Stdout)) {
        *r += write(Stream::Stdout, b"\n");
    }
}

/// `ebegin()`: start of a "doing something ..." line; finish with `eend`.
pub fn ebegin(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| !is_quiet()) else { return 0 };
    let mut r = einfovn_(msg);
    ebegin_tail(&mut r);
    lastcmd("ebegin");
    r
}

/// `_eend()`: print `[ msg ]` right-aligned (or after the line, no color).
fn eend_bracket(fp: Stream, col: i32, color: i32, msg: Option<&[u8]>) {
    let Some(msg) = msg else { return };
    // C computes this in size_t and truncates to int.
    let mut cols = (i64::from(get_term_columns(fp)) - (msg.len() as i64 + 5)) as i32;

    {
        let mut st = state();
        if st.term.is_none() {
            st.term = env::get("TERM");
            st.term_is_cons25 = st.term.as_deref() == Some(b"cons25");
        }
        if st.term_is_cons25 {
            cols -= 1;
        }
    }

    if cols > 0 && colour_terminal(Some(fp)) {
        let up = state().caps.map_or(c"", |c| c.up);
        let goto = format!("\x1b[{cols}C");
        // QUIRK: the colors come from ecolor(), which guesses the tty,
        // rather than from fp.
        let line = bcat!(
            up,
            goto,
            " ",
            ecolor(EColor::Bracket),
            "[",
            ecolor_raw(color),
            " ",
            msg,
            " ",
            ecolor(EColor::Bracket),
            "]",
            ecolor(EColor::Normal),
            "\n"
        );
        write(fp, &line);
    } else {
        if col > 0 {
            for _ in 0..(cols - col).max(0) {
                write(fp, b" ");
            }
        }
        write(fp, &bcat!(" [ ", msg, " ]\n"));
    }
}

/// `_do_eend()`.  `warn` selects ewend (yellow) over eend (red) messages.
fn do_eend(warn: bool, retval: i32, msg: Option<&[u8]>) -> i32 {
    let mut col = 0;
    let mut fp = Stream::Stdout;
    if let Some(m) = msg.filter(|m| !m.is_empty() && retval != 0) {
        fp = Stream::Stderr;
        col = if warn { ewarnvn_(m) } else { eerrorvn_(m) };
        col += write(fp, b"\n");
    }
    let (color, text) = if retval == 0 { (EColor::Good, OK) } else { (EColor::Bad, NOT_OK) };
    eend_bracket(fp, col, color as i32, Some(text));
    retval
}

/// `eend()`: finish an `ebegin` with `[ ok ]` (retval 0) or `[ !! ]`,
/// printing `msg` as an error first when retval is non-zero.
pub fn eend(retval: i32, msg: Option<&[u8]>) -> i32 {
    if is_quiet() {
        return retval;
    }
    do_eend(false, retval, msg);
    lastcmd("eend");
    retval
}

/// `ewend()`: like `eend` but the message is a warning.
pub fn ewend(retval: i32, msg: Option<&[u8]>) -> i32 {
    if is_quiet() {
        return retval;
    }
    do_eend(true, retval, msg);
    lastcmd("ewend");
    retval
}

/// `ebracket()`: print `[ msg ]` at the end of the line in `color`.
pub fn ebracket(col: i32, color: EColor, msg: Option<&[u8]>) {
    eend_bracket(Stream::Stdout, col, color as i32, msg);
}

/// `ebracket()` with a raw C enum value (unknown colors print as "").
pub fn ebracket_raw(col: i32, color: i32, msg: Option<&[u8]>) {
    eend_bracket(Stream::Stdout, col, color, msg);
}

/// `eindent()`: increase `$EINFO_INDENT` by 2.
pub fn eindent() {
    let mut amount = 0;
    if let Some(e) = env::get("EINFO_INDENT") {
        let p = strtoimax(&e, 0);
        amount = if p.erange { 0 } else { p.value as i32 };
    }
    amount = amount.wrapping_add(INDENT_WIDTH);
    env::set("EINFO_INDENT", format!("{amount:08}").as_bytes());
}

/// `eoutdent()`: decrease `$EINFO_INDENT` by 2, unsetting it at 0.
pub fn eoutdent() {
    let Some(e) = env::get("EINFO_INDENT") else { return };
    let p = strtoimax(&e, 0);
    let amount = if p.erange { 0 } else { (p.value as i32).wrapping_sub(INDENT_WIDTH) };
    if amount <= 0 {
        env::unset("EINFO_INDENT");
    } else {
        env::set("EINFO_INDENT", format!("{amount:08}").as_bytes());
    }
}

/// `einfovn()`: `einfon` only if `EINFO_VERBOSE`.
pub fn einfovn(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| is_verbose()) else { return 0 };
    let r = einfovn_(msg);
    lastcmd("einfovn");
    r
}

/// `ewarnvn()`: `ewarnn` only if `EINFO_VERBOSE`.
pub fn ewarnvn(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| is_verbose()) else { return 0 };
    let r = ewarnvn_(msg);
    lastcmd("ewarnvn");
    r
}

/// `einfov()`: `einfo` only if `EINFO_VERBOSE`.
pub fn einfov(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| is_verbose()) else { return 0 };
    let mut r = einfovn_(msg);
    r += write(Stream::Stdout, b"\n");
    lastcmd("einfov");
    r
}

/// `ewarnv()`: `ewarn` only if `EINFO_VERBOSE`.
pub fn ewarnv(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| is_verbose()) else { return 0 };
    let mut r = ewarnvn_(msg);
    // QUIRK: the message goes to stderr but its newline to stdout, and
    // unlike ewarn it is not logged.
    r += write(Stream::Stdout, b"\n");
    lastcmd("ewarnv");
    r
}

/// `ebeginv()`: `ebegin` only if `EINFO_VERBOSE`.
pub fn ebeginv(msg: Option<&[u8]>) -> i32 {
    let Some(msg) = msg.filter(|_| is_verbose()) else { return 0 };
    let mut r = einfovn_(msg);
    ebegin_tail(&mut r);
    lastcmd("ebeginv");
    r
}

/// `eendv()`: `eend` only if `EINFO_VERBOSE`.
pub fn eendv(retval: i32, msg: Option<&[u8]>) -> i32 {
    // QUIRK: returns 0, not retval, when not verbose.
    if !is_verbose() {
        return 0;
    }
    do_eend(false, retval, msg);
    lastcmd("eendv");
    retval
}

/// `ewendv()`: `ewend` only if `EINFO_VERBOSE`.
pub fn ewendv(retval: i32, msg: Option<&[u8]>) -> i32 {
    if !is_verbose() {
        return 0;
    }
    // QUIRK: upstream passes "ewendv" to _do_eend, which only checks for
    // "ewend", so the message is printed as an error (red), not a warning.
    do_eend(false, retval, msg);
    lastcmd("ewendv");
    retval
}

/// `eindentv()`: `eindent` only if `EINFO_VERBOSE`.
pub fn eindentv() {
    if is_verbose() {
        eindent();
    }
}

/// `eoutdentv()`: `eoutdent` only if `EINFO_VERBOSE`.
pub fn eoutdentv() {
    if is_verbose() {
        eoutdent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yesno_variants() {
        assert_eq!(yesno(Some(b"On")), (true, true));
        assert_eq!(yesno(Some(b"OFF")), (false, true));
        assert_eq!(yesno(Some(b"maybe")), (false, false));
        assert!(noyes(Some(b"no")));
        assert!(!noyes(Some(b"yes")));
        assert!(!noyes(Some(b"good=2;1:")));
        assert!(!noyes(None));
    }
}

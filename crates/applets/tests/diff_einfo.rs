//! einfo family vs the C reference.
//!
//! Not covered on purpose: anything that reaches syslog (`esyslog` with
//! valid arguments, or `EINFO_LOG` set), because esyslog logs at LOG_EMERG,
//! which syslog daemons broadcast to every terminal.

mod common;
use common::Case;

const MSG_APPLETS: &[&str] =
    &["einfo", "einfon", "ewarn", "ewarnn", "eerror", "eerrorn", "ebegin", "veinfo", "vewarn", "vebegin"];
const END_APPLETS: &[&str] = &["eend", "ewend", "veend", "vewend"];
const QUIET_APPLETS: &[&str] = &["eindent", "eoutdent", "veindent", "veoutdent"];

#[test]
fn messages() {
    let mut cases = Vec::new();
    for &a in MSG_APPLETS {
        cases.push(Case::new(a, &[]));
        cases.push(Case::new(a, &["hello"]));
        cases.push(Case::new(a, &["hello", "big", "world"]));
        cases.push(Case::new(a, &[""]));
        cases.push(Case::new(a, &["two", "words"]).env("EINFO_VERBOSE", "yes"));
        cases.push(Case::new(a, &["quiet"]).env("EINFO_QUIET", "YES"));
        cases.push(Case::new(a, &["quiet"]).env("EINFO_QUIET", "on"));
        cases.push(Case::new(a, &["really quiet"]).env("EERROR_QUIET", "true"));
        cases.push(Case::new(a, &["after n"]).env("EINFO_LASTCMD", "einfon"));
        cases.push(Case::new(a, &["after ewarn"]).env("EINFO_LASTCMD", "ewarn"));
        cases.push(Case::new(a, &["percent %s %d %n"]));
    }
    check_all_named("messages", cases);
}

#[test]
fn indentation() {
    let mut cases = Vec::new();
    for indent in ["2", "00000004", "00000008", "00000010", "0x6", "-5", "100", "abc", "", "99999999999999999999"] {
        cases.push(Case::new("einfo", &["indented"]).env("EINFO_INDENT", indent));
        cases.push(Case::new("eend", &["1", "indented"]).env("EINFO_INDENT", indent));
    }
    for &a in QUIET_APPLETS {
        cases.push(Case::new(a, &[]));
        cases.push(Case::new(a, &["ignored", "args"]).env("EINFO_INDENT", "4"));
        cases.push(Case::new(a, &[]).env("EINFO_VERBOSE", "1"));
    }
    check_all_named("indentation", cases);
}

#[test]
fn eend_family() {
    let mut cases = Vec::new();
    for &a in END_APPLETS {
        for args in [
            &[][..],
            &["0"],
            &["1"],
            &["-1"],
            &["256"],
            &["0x101"],
            &["010"],
            &[""],
            &["abc", "def"],
            &["1", "it", "broke"],
            &["0", "ignored", "message"],
            &["99999999999999999999"],
            &[" 3"],
            &["3 "],
        ] {
            cases.push(Case::new(a, args));
            cases.push(Case::new(a, args).env("EINFO_VERBOSE", "yes"));
        }
        for cols in ["10", "3", "", "abc", "200", "-4"] {
            cases.push(Case::new(a, &["1", "narrow", "terminal"]).env("COLUMNS", cols).env("EINFO_VERBOSE", "y"));
        }
        cases.push(Case::new(a, &["1", "cons"]).env("TERM", "cons25"));
        cases.push(Case::new(a, &["1", "q"]).env("EINFO_QUIET", "yes"));
    }
    check_all_named("eend", cases);
}

#[test]
fn colors() {
    let mut cases = Vec::new();
    for term in [None, Some("xterm"), Some("dumb"), Some("xterm-256color"), Some("cons25"), Some("linux"), Some("")] {
        let mut c = Case::new("eval_ecolors", &[]);
        if let Some(t) = term {
            c = c.env("TERM", t);
        }
        cases.push(c.clone());
        cases.push(c.clone().env("EINFO_COLOR", "no"));
        cases.push(c.clone().env("EINFO_COLOR", "good=5;0:bad=7;1:warn=11:"));
        cases.push(c.clone().env("EINFO_COLOR", "bracket=-1;0"));
        cases.push(c.pty());
    }
    check_all_named("colors", cases);
}

#[test]
fn on_a_terminal() {
    let mut cases = Vec::new();
    for term in ["xterm", "dumb", "cons25"] {
        for &a in MSG_APPLETS {
            cases.push(Case::new(a, &["on", "a", "tty"]).env("TERM", term).env("EINFO_VERBOSE", "yes").pty());
        }
        for &a in END_APPLETS {
            cases.push(Case::new(a, &["0"]).env("TERM", term).env("EINFO_VERBOSE", "yes").pty());
            cases.push(Case::new(a, &["1", "failed"]).env("TERM", term).env("EINFO_VERBOSE", "yes").pty());
            cases.push(Case::new(a, &["1", "x"]).env("TERM", term).env("COLUMNS", "4").pty());
        }
        cases.push(Case::new("einfo", &["no color"]).env("TERM", term).env("EINFO_COLOR", "off").pty());
        cases.push(Case::new("einfo", &["custom"]).env("TERM", term).env("EINFO_COLOR", "good=3;0").pty());
        cases.push(Case::new("ebegin", &["wide"]).env("TERM", term).env("COLUMNS", "120").pty());
    }
    check_all_named("tty", cases);
}

#[test]
fn ewaitfile_and_esyslog_errors() {
    let cases = vec![
        Case::new("ewaitfile", &[]),
        Case::new("ewaitfile", &["5"]),
        Case::new("ewaitfile", &["x", "/"]),
        Case::new("ewaitfile", &["5", "/", "/etc"]),
        Case::new("ewaitfile", &["5", "/"]).env("EINFO_VERBOSE", "yes"),
        Case::new("ewaitfile", &["1", "/nonexistent/openrc-rs-test"]),
        Case::new("ewaitfile", &["1", "/nonexistent/openrc-rs-test"]).env("EINFO_VERBOSE", "yes"),
        Case::new("esyslog", &[]),
        Case::new("esyslog", &["daemon"]),
        Case::new("esyslog", &["daemon.bogus", "tag", "msg"]),
        Case::new("esyslog", &["daemon.notice"]),
        Case::new("esyslog", &["daemon.notice", "tag"]),
        Case::new("esyslog", &["user.", "tag", "msg"]),
    ];
    check_all_named("ewaitfile", cases);
}

fn check_all_named(group: &str, cases: Vec<Case>) {
    common::check_all(group, &cases);
}

//! `is_newer_than` / `is_older_than`: ports of `src/is_newer_than/` and
//! `src/is_older_than/`.  `is_newer_than SOURCE TARGET...` succeeds if
//! nothing under any TARGET is newer than SOURCE.

use rc::mtime::rc_newer_than;

pub fn is_newer_than(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    if args.len() < 3 {
        return 1;
    }
    for target in &args[2..] {
        if !rc_newer_than(&args[1], target).ok {
            return 1;
        }
    }
    0
}

/// QUIRK: upstream ("this test is perverted") also uses rc_newer_than and
/// succeeds as soon as any TARGET contains something newer than SOURCE;
/// it never calls rc_older_than.
pub fn is_older_than(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    if args.len() < 3 {
        return 1;
    }
    for target in &args[2..] {
        if !rc_newer_than(&args[1], target).ok {
            return 0;
        }
    }
    1
}

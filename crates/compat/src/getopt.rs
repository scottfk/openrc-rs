//! A reimplementation of glibc's `getopt_long()`.
//!
//! OpenRC's option parsing (and its error messages, which glibc prints) is
//! user-visible, so this follows glibc's `_getopt_internal_r` closely:
//! argument permutation, `POSIXLY_CORRECT`, `+`/`-`/`:` optstring prefixes,
//! unambiguous long-option abbreviations, and the exact diagnostic texts.

use crate::bcat;
use crate::stdio;

/// `has_arg` of a `struct option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HasArg {
    No,
    Required,
    Optional,
}

/// A `struct option` (without the `flag` pointer, which OpenRC never uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongOpt {
    pub name: &'static str,
    pub has_arg: HasArg,
    pub val: i32,
}

impl LongOpt {
    pub const fn new(name: &'static str, has_arg: HasArg, val: u8) -> LongOpt {
        LongOpt { name, has_arg, val: val as i32 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ordering {
    RequireOrder,
    Permute,
    ReturnInOrder,
}

/// Parser state (`optind`, `optarg`, `optopt` and glibc's private fields).
#[derive(Debug)]
pub struct Getopt {
    /// argv.  Permuted in place, as glibc does.
    pub args: Vec<Vec<u8>>,
    pub optind: usize,
    pub optarg: Option<Vec<u8>>,
    pub optopt: i32,
    /// `opterr`.
    pub opterr: bool,
    optstring: Vec<u8>,
    ordering: Ordering,
    /// `__nextchar`: (index into args, byte offset).
    nextchar: Option<(usize, usize)>,
    first_nonopt: usize,
    last_nonopt: usize,
}

impl Getopt {
    /// Equivalent of the first `getopt_long()` call with `optind == 1`.
    pub fn new(args: Vec<Vec<u8>>, optstring: &str) -> Getopt {
        let mut s = optstring.as_bytes();
        let ordering = match s.first() {
            Some(b'-') => {
                s = &s[1..];
                Ordering::ReturnInOrder
            }
            Some(b'+') => {
                s = &s[1..];
                Ordering::RequireOrder
            }
            _ if std::env::var_os("POSIXLY_CORRECT").is_some() => Ordering::RequireOrder,
            _ => Ordering::Permute,
        };
        Getopt {
            args,
            optind: 1,
            optarg: None,
            optopt: b'?' as i32,
            opterr: true,
            optstring: s.to_vec(),
            ordering,
            nextchar: None,
            first_nonopt: 1,
            last_nonopt: 1,
        }
    }

    fn argc(&self) -> usize {
        self.args.len()
    }

    fn print_errors(&self) -> bool {
        self.opterr && self.optstring.first() != Some(&b':')
    }

    fn missing_arg_code(&self) -> i32 {
        if self.optstring.first() == Some(&b':') { b':' as i32 } else { b'?' as i32 }
    }

    fn argv0(&self) -> &[u8] {
        self.args.first().map_or(&[][..], |a| a.as_slice())
    }

    /// glibc `NONOPTION_P`.
    fn nonoption(&self, i: usize) -> bool {
        let a = &self.args[i];
        a.first() != Some(&b'-') || a.len() == 1
    }

    /// glibc `exchange()`: move the non-options in
    /// `[first_nonopt, last_nonopt)` after the options in `[last_nonopt, optind)`.
    fn exchange(&mut self) {
        let (bottom, middle, top) = (self.first_nonopt, self.last_nonopt, self.optind);
        self.args[bottom..top].rotate_left(middle - bottom);
        self.first_nonopt += top - middle;
        self.last_nonopt = top;
    }

    fn nextchar_rest(&self) -> &[u8] {
        match self.nextchar {
            Some((i, off)) => &self.args[i][off..],
            None => &[],
        }
    }

    /// Short-options only (`getopt()`).
    pub fn next_short(&mut self) -> Option<i32> {
        self.next_long(&[])
    }

    /// One `getopt_long()` call.  `None` is glibc's `-1`.
    pub fn next_long(&mut self, longopts: &[LongOpt]) -> Option<i32> {
        if self.argc() < 1 {
            return None;
        }
        self.optarg = None;

        if self.nextchar_rest().is_empty() {
            // Advance to the next ARGV-element.
            if self.last_nonopt > self.optind {
                self.last_nonopt = self.optind;
            }
            if self.first_nonopt > self.optind {
                self.first_nonopt = self.optind;
            }

            if self.ordering == Ordering::Permute {
                if self.first_nonopt != self.last_nonopt && self.last_nonopt != self.optind {
                    self.exchange();
                } else if self.last_nonopt != self.optind {
                    self.first_nonopt = self.optind;
                }
                while self.optind < self.argc() && self.nonoption(self.optind) {
                    self.optind += 1;
                }
                self.last_nonopt = self.optind;
            }

            if self.optind != self.argc() && self.args[self.optind] == b"--" {
                self.optind += 1;
                if self.first_nonopt != self.last_nonopt && self.last_nonopt != self.optind {
                    self.exchange();
                } else if self.first_nonopt == self.last_nonopt {
                    self.first_nonopt = self.optind;
                }
                self.last_nonopt = self.argc();
                self.optind = self.argc();
            }

            if self.optind == self.argc() {
                if self.first_nonopt != self.last_nonopt {
                    self.optind = self.first_nonopt;
                }
                return None;
            }

            if self.nonoption(self.optind) {
                if self.ordering == Ordering::RequireOrder {
                    return None;
                }
                self.optarg = Some(self.args[self.optind].clone());
                self.optind += 1;
                return Some(1);
            }

            if !longopts.is_empty() && self.args[self.optind].get(1) == Some(&b'-') {
                self.nextchar = Some((self.optind, 2));
                return Some(self.process_long_option(longopts));
            }

            self.nextchar = Some((self.optind, 1));
        }

        // Short option character.
        let (ai, off) = self.nextchar.expect("nextchar set above");
        let c = self.args[ai][off];
        self.nextchar = Some((ai, off + 1));
        let at_end = off + 1 >= self.args[ai].len();
        if at_end {
            self.optind += 1;
        }

        let spec = self.optstring.iter().position(|&b| b == c);
        let Some(spec) = spec.filter(|_| c != b':' && c != b';') else {
            if self.print_errors() {
                stdio::err(&bcat!(self.argv0(), ": invalid option -- '", [c], "'\n"));
            }
            self.optopt = i32::from(c);
            return Some(b'?' as i32);
        };

        let takes = self.optstring.get(spec + 1) == Some(&b':');
        let optional = takes && self.optstring.get(spec + 2) == Some(&b':');
        if takes {
            if optional {
                if !at_end {
                    self.optarg = Some(self.args[ai][off + 1..].to_vec());
                    self.optind += 1;
                }
            } else if !at_end {
                self.optarg = Some(self.args[ai][off + 1..].to_vec());
                self.optind += 1;
            } else if self.optind == self.argc() {
                if self.print_errors() {
                    stdio::err(&bcat!(self.argv0(), ": option requires an argument -- '", [c], "'\n"));
                }
                self.optopt = i32::from(c);
                self.nextchar = None;
                return Some(self.missing_arg_code());
            } else {
                self.optarg = Some(self.args[self.optind].clone());
                self.optind += 1;
            }
            self.nextchar = None;
        }
        Some(i32::from(c))
    }

    /// glibc `process_long_option()` with prefix `--`.
    fn process_long_option(&mut self, longopts: &[LongOpt]) -> i32 {
        let rest = self.nextchar_rest().to_vec();
        let nameend = rest.iter().position(|&b| b == b'=').unwrap_or(rest.len());
        let name = &rest[..nameend];

        // Exact match first.
        let mut found = longopts.iter().position(|o| o.name.as_bytes() == name);

        if found.is_none() {
            // Abbreviations: the first prefix match, and the set of later
            // matches that differ from it (in has_arg or val).
            let mut first: Option<usize> = None;
            let mut ambig: Vec<usize> = Vec::new();
            for (i, o) in longopts.iter().enumerate() {
                if !o.name.as_bytes().starts_with(name) {
                    continue;
                }
                match first {
                    None => first = Some(i),
                    Some(f) => {
                        let p = &longopts[f];
                        if p.has_arg != o.has_arg || p.val != o.val {
                            if ambig.is_empty() {
                                ambig.push(f);
                            }
                            ambig.push(i);
                        }
                    }
                }
            }
            if !ambig.is_empty() {
                if self.print_errors() {
                    let mut msg = bcat!(self.argv0(), ": option '--", rest, "' is ambiguous; possibilities:");
                    for i in ambig {
                        msg.extend_from_slice(&bcat!(" '--", longopts[i].name, "'"));
                    }
                    msg.push(b'\n');
                    stdio::err(&msg);
                }
                self.nextchar = None;
                self.optind += 1;
                self.optopt = 0;
                return b'?' as i32;
            }
            found = first;
        }

        let Some(idx) = found else {
            if self.print_errors() {
                stdio::err(&bcat!(self.argv0(), ": unrecognized option '--", rest, "'\n"));
            }
            self.nextchar = None;
            self.optind += 1;
            self.optopt = 0;
            return b'?' as i32;
        };

        let opt = longopts[idx];
        self.optind += 1;
        self.nextchar = None;
        if nameend < rest.len() {
            if opt.has_arg != HasArg::No {
                self.optarg = Some(rest[nameend + 1..].to_vec());
            } else {
                if self.print_errors() {
                    stdio::err(&bcat!(self.argv0(), ": option '--", opt.name, "' doesn't allow an argument\n"));
                }
                self.optopt = opt.val;
                return b'?' as i32;
            }
        } else if opt.has_arg == HasArg::Required {
            if self.optind < self.argc() {
                self.optarg = Some(self.args[self.optind].clone());
                self.optind += 1;
            } else {
                if self.print_errors() {
                    stdio::err(&bcat!(self.argv0(), ": option '--", opt.name, "' requires an argument\n"));
                }
                self.optopt = opt.val;
                return self.missing_arg_code();
            }
        }
        opt.val
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<Vec<u8>> {
        a.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    const LONG: &[LongOpt] = &[
        LongOpt::new("directory", HasArg::No, b'd'),
        LongOpt::new("directory-truncate", HasArg::No, b'D'),
        LongOpt::new("mode", HasArg::Required, b'm'),
    ];

    #[test]
    fn permutes_nonoptions_to_the_end() {
        let mut g = Getopt::new(args(&["x", "a", "-d", "b", "-m", "755", "c"]), "dDm:");
        let mut seen = Vec::new();
        while let Some(c) = g.next_long(LONG) {
            seen.push((c as u8 as char, g.optarg.clone()));
        }
        assert_eq!(seen, vec![('d', None), ('m', Some(b"755".to_vec()))]);
        assert_eq!(&g.args[g.optind..], &args(&["a", "b", "c"])[..]);
    }

    #[test]
    fn long_options() {
        let mut g = Getopt::new(args(&["x", "--mode=644", "--directory", "--", "-d"]), "dDm:");
        assert_eq!(g.next_long(LONG), Some(b'm' as i32));
        assert_eq!(g.optarg.as_deref(), Some(&b"644"[..]));
        assert_eq!(g.next_long(LONG), Some(b'd' as i32));
        assert_eq!(g.next_long(LONG), None);
        assert_eq!(&g.args[g.optind..], &args(&["-d"])[..]);
    }

    #[test]
    fn abbreviations() {
        // "--dir" is an exact prefix of two options with different vals.
        let mut g = Getopt::new(args(&["x", "--dir"]), "dDm:");
        g.opterr = false;
        assert_eq!(g.next_long(LONG), Some(b'?' as i32));
        let mut g = Getopt::new(args(&["x", "--mo", "1"]), "dDm:");
        assert_eq!(g.next_long(LONG), Some(b'm' as i32));
        assert_eq!(g.optarg.as_deref(), Some(&b"1"[..]));
        // An exact match wins over longer options sharing the prefix.
        let mut g = Getopt::new(args(&["x", "--directory"]), "dDm:");
        assert_eq!(g.next_long(LONG), Some(b'd' as i32));
    }

    #[test]
    fn clustered_short_options() {
        let mut g = Getopt::new(args(&["x", "-dm644", "-Dm", "7"]), "dDm:");
        assert_eq!(g.next_long(LONG), Some(b'd' as i32));
        assert_eq!(g.next_long(LONG), Some(b'm' as i32));
        assert_eq!(g.optarg.as_deref(), Some(&b"644"[..]));
        assert_eq!(g.next_long(LONG), Some(b'D' as i32));
        assert_eq!(g.next_long(LONG), Some(b'm' as i32));
        assert_eq!(g.optarg.as_deref(), Some(&b"7"[..]));
        assert_eq!(g.next_long(LONG), None);
    }
}

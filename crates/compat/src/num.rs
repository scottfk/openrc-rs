//! Number parsing with glibc `strto*` semantics.
//!
//! These matter for compatibility: e.g. `EINFO_INDENT` is parsed with base 0,
//! so `"00000010"` is octal (8), and `"00000008"` stops at the `8` and is 0.

/// `isspace()` in the C locale.
pub fn is_c_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Result of a `strto*` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed<T> {
    pub value: T,
    /// Byte offset of `*endptr`.  0 when no conversion was performed
    /// (C sets `endptr = nptr` in that case, even if whitespace was skipped).
    pub end: usize,
    /// `errno == ERANGE`.
    pub erange: bool,
}

impl<T> Parsed<T> {
    /// The whole string was consumed (`*endptr == '\0'`).
    pub fn complete(&self, s: &[u8]) -> bool {
        self.end == s.len()
    }
}

fn digit_value(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some(u32::from(b - b'0')),
        b'a'..=b'z' => Some(u32::from(b - b'a') + 10),
        b'A'..=b'Z' => Some(u32::from(b - b'A') + 10),
        _ => None,
    }
}

/// Shared scanner: returns (negative, magnitude, overflowed, end).
fn scan(s: &[u8], base: u32) -> (bool, u64, bool, usize) {
    assert!(base == 0 || (2..=36).contains(&base), "invalid base {base}");
    let mut i = 0;
    while i < s.len() && is_c_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut base = base;
    let hex_prefix = i + 1 < s.len()
        && s[i] == b'0'
        && (s[i + 1] | 0x20) == b'x'
        && s.get(i + 2).and_then(|&b| digit_value(b)).is_some_and(|d| d < 16);
    if (base == 0 || base == 16) && hex_prefix {
        base = 16;
        i += 2;
    } else if base == 0 {
        base = if i < s.len() && s[i] == b'0' { 8 } else { 10 };
    }

    let start = i;
    let mut acc: u64 = 0;
    let mut overflow = false;
    while i < s.len() {
        let Some(d) = digit_value(s[i]).filter(|&d| d < base) else {
            break;
        };
        match acc.checked_mul(u64::from(base)).and_then(|v| v.checked_add(u64::from(d))) {
            Some(v) => acc = v,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        // No digits: no conversion at all.
        return (false, 0, false, 0);
    }
    (neg, acc, overflow, i)
}

/// `strtoimax()` / `strtol()` (both 64-bit on Linux).
pub fn strtoimax(s: &[u8], base: u32) -> Parsed<i64> {
    let (neg, mag, overflow, end) = scan(s, base);
    let (value, erange) = if overflow {
        (if neg { i64::MIN } else { i64::MAX }, true)
    } else if neg {
        if mag > 1u64 << 63 { (i64::MIN, true) } else { ((mag as i64).wrapping_neg(), false) }
    } else if mag > i64::MAX as u64 {
        (i64::MAX, true)
    } else {
        (mag as i64, false)
    };
    Parsed { value, end, erange }
}

/// `strtoul()` (64-bit on Linux).  A leading `-` negates in unsigned arithmetic.
pub fn strtoul(s: &[u8], base: u32) -> Parsed<u64> {
    let (neg, mag, overflow, end) = scan(s, base);
    if overflow {
        return Parsed { value: u64::MAX, end, erange: true };
    }
    let value = if neg { mag.wrapping_neg() } else { mag };
    Parsed { value, end, erange: false }
}

/// `atoi()`: glibc defines it as `(int) strtol(s, NULL, 10)`.
pub fn atoi(s: &[u8]) -> i32 {
    strtoimax(s, 10).value as i32
}

/// `sscanf(s, "%d", &n) == 1`: glibc converts with `strtol` and stores the
/// truncated `int`.  Returns `None` if no digits matched.
pub fn scanf_int(s: &[u8]) -> Option<i32> {
    let p = strtoimax(s, 10);
    (p.end > 0).then_some(p.value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base0() {
        assert_eq!(strtoimax(b"00000010", 0).value, 8);
        let p = strtoimax(b"00000008", 0);
        assert_eq!((p.value, p.end), (0, 7));
        assert_eq!(strtoimax(b"0x1f", 0).value, 31);
        assert_eq!(strtoimax(b"  -12abc", 0), Parsed { value: -12, end: 5, erange: false });
        // "0x" without a hex digit parses the 0 and stops at x.
        assert_eq!(strtoimax(b"0xg", 0), Parsed { value: 0, end: 1, erange: false });
    }

    #[test]
    fn no_conversion() {
        assert_eq!(strtoimax(b"", 0), Parsed { value: 0, end: 0, erange: false });
        assert_eq!(strtoimax(b"  x", 10), Parsed { value: 0, end: 0, erange: false });
        assert_eq!(strtoimax(b"-", 10).end, 0);
    }

    #[test]
    fn range() {
        assert_eq!(strtoimax(b"9223372036854775807", 10).value, i64::MAX);
        assert!(strtoimax(b"9223372036854775808", 10).erange);
        let p = strtoimax(b"-9223372036854775808", 10);
        assert_eq!((p.value, p.erange), (i64::MIN, false));
        assert!(strtoimax(b"-9223372036854775809", 10).erange);
        assert_eq!(strtoul(b"-1", 8).value, u64::MAX);
        assert!(strtoul(b"99999999999999999999999", 10).erange);
    }

    #[test]
    fn small() {
        assert_eq!(atoi(b" 42x"), 42);
        assert_eq!(atoi(b"4294967298"), 2);
        assert_eq!(scanf_int(b"12ab"), Some(12));
        assert_eq!(scanf_int(b"ab"), None);
        assert_eq!(strtoul(b"0755", 8).value, 0o755);
    }
}

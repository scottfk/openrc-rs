//! `shell_var`: port of `src/shell_var/shell_var.c`.  Turns each argument
//! into a valid shell variable name (non-alphanumerics become `_`).

use openrc_compat::stdio;

pub fn main(_applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    let mut out = Vec::new();
    for (i, arg) in args.iter().enumerate().skip(1) {
        if i != 1 {
            out.push(b' ');
        }
        // isalnum() in the C locale; the argument is a C string.
        out.extend(
            openrc_compat::bytes::c_str_prefix(arg).iter().map(|&c| if c.is_ascii_alphanumeric() { c } else { b'_' }),
        );
    }
    out.push(b'\n');
    stdio::out(&out);
    0
}

//! libeinfo.so.1 C ABI vs the C reference library: the same C program
//! (tests/capi/einfo-api.c) linked against each.
//!
//! Needs `make capi` first; skipped (or failed, with OPENRC_REF_REQUIRED)
//! if our library has not been linked.

mod common;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Case;

/// Compile the test program against the libeinfo in `libdir`.
fn build(out: &Path, libdir: &Path) {
    let root = common::repo_root();
    let st = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()))
        .arg("-o")
        .arg(out)
        .arg(root.join("tests/capi/einfo-api.c"))
        .arg(format!("-I{}", root.join("crates/capi-einfo/include").display()))
        .arg(format!("-L{}", libdir.display()))
        .arg(format!("-Wl,-rpath,{}", libdir.display()))
        .arg("-leinfo")
        .status()
        .unwrap();
    assert!(st.success(), "compiling einfo-api.c against {}", libdir.display());
}

#[test]
fn einfo_c_abi() {
    let Some(refdir) = common::reference_dir() else { return };
    let ours = common::repo_root().join("target/capi");
    if !ours.join("libeinfo.so.1").exists() {
        if std::env::var_os("OPENRC_REF_REQUIRED").is_some() {
            panic!("target/capi/libeinfo.so.1 missing; run `make capi`");
        }
        eprintln!("SKIPPED: target/capi/libeinfo.so.1 missing; run `make capi`");
        return;
    }
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let (cprog, rprog) = (tmp.join("einfo-api-c"), tmp.join("einfo-api-rust"));
    build(&cprog, &refdir.join("libeinfo"));
    build(&rprog, &ours);

    let prog = |args: &[&str]| Case::programs("einfo-api", cprog.clone(), rprog.clone(), args);
    let mut cases = Vec::new();
    for mode in ["", "eerrorx", "ewarnx", "eerrorx-null"] {
        cases.push(prog(&[mode]));
        cases.push(prog(&[mode]).env("EINFO_VERBOSE", "yes"));
        cases.push(prog(&[mode]).env("EINFO_QUIET", "yes"));
        cases.push(prog(&[mode]).env("EERROR_QUIET", "yes"));
        cases.push(prog(&[mode]).env("TERM", "xterm"));
        cases.push(prog(&[mode]).env("TERM", "xterm").env("EINFO_VERBOSE", "yes").pty());
        cases.push(prog(&[mode]).env("TERM", "dumb").pty());
        cases.push(prog(&[mode]).env("TERM", "cons25").env("COLUMNS", "50").pty());
        cases.push(prog(&[mode]).env("TERM", "linux").env("EINFO_COLOR", "good=6;0:warn=1").pty());
        cases.push(prog(&[mode]).env("EINFO_INDENT", "00000010").env("EINFO_LASTCMD", "ebegin"));
    }
    common::check_all("capi-einfo", &cases);
}

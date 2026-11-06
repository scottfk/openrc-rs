//! Differential test harness: run the C reference build of OpenRC and
//! openrc-rs on the same input and require identical behaviour.
//!
//! Compared: stdout, stderr, exit status, and (for cases with a sandbox)
//! the resulting filesystem tree.  Both run with the same `argv[0]`, a
//! clean environment, and the same sandbox path, one after the other.
//!
//! The reference is built by `tools/build-reference.sh` (`make reference`).
//! Without it these tests are skipped, unless `OPENRC_REF_REQUIRED=1`.

#![allow(dead_code)]

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const OURS: &str = env!("CARGO_BIN_EXE_openrc-rs");

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

/// The reference build directory, or None (skip) if it has not been built.
pub fn reference_dir() -> Option<PathBuf> {
    let root = repo_root();
    let tag = std::fs::read_to_string(root.join("tools/REFERENCE_TAG")).unwrap();
    let dir = root.join("target/reference").join(tag.trim()).join("build/src");
    if dir.join("checkpath/checkpath").exists() {
        return Some(dir);
    }
    if std::env::var_os("OPENRC_REF_REQUIRED").is_some() {
        panic!("reference build missing at {}; run `make reference`", dir.display());
    }
    eprintln!("SKIPPED: reference build missing; run `make reference`");
    None
}

/// Where the reference installs-from-build an applet.
fn reference_bin(refdir: &Path, applet: &str) -> PathBuf {
    match applet {
        "checkpath" => refdir.join("checkpath/checkpath"),
        _ => refdir.join("einfo").join(applet),
    }
}

/// One differential test case.
#[derive(Clone)]
pub struct Case {
    pub applet: &'static str,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Run with stdin/stdout/stderr on a pseudo-terminal (output merged).
    pub pty: bool,
    /// Create a sandbox directory, populated by this function; `$ROOT` in
    /// args is replaced by its path, and its final state is compared.
    pub setup: Option<fn(&Path)>,
    /// Explicit (C, Rust) programs instead of the applet's binaries.
    pub bins: Option<(PathBuf, PathBuf)>,
}

impl Case {
    pub fn new(applet: &'static str, args: &[&str]) -> Case {
        Case {
            applet,
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            pty: false,
            setup: None,
            bins: None,
        }
    }
    pub fn env(mut self, k: &str, v: &str) -> Case {
        self.env.push((k.into(), v.into()));
        self
    }
    pub fn pty(mut self) -> Case {
        self.pty = true;
        self
    }
    /// Compare two given programs (run as `argv[0] = applet`).
    pub fn programs(applet: &'static str, c: PathBuf, rust: PathBuf, args: &[&str]) -> Case {
        let mut case = Case::new(applet, args);
        case.bins = Some((c, rust));
        case
    }
    pub fn sandbox(mut self, setup: fn(&Path)) -> Case {
        self.setup = Some(setup);
        self
    }
    fn describe(&self) -> String {
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
        format!(
            "{}{} {}{}",
            if env.is_empty() { String::new() } else { env.join(" ") + " " },
            self.applet,
            self.args.join(" "),
            if self.pty { "  [pty]" } else { "" }
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: String,
    tree: Vec<String>,
}

fn status_string(s: std::process::ExitStatus) -> String {
    match (s.code(), s.signal()) {
        (Some(c), _) => format!("exit {c}"),
        (None, Some(sig)) => format!("signal {sig}"),
        _ => "unknown".into(),
    }
}

/// Recursive listing: path, type, mode, owner, size / link target.
fn snapshot(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let ft = m.file_type();
            let (kind, extra) = if ft.is_symlink() {
                ("l", format!("-> {}", std::fs::read_link(&p).unwrap().display()))
            } else if ft.is_dir() {
                ("d", String::new())
            } else if ft.is_file() {
                ("f", format!("size={}", m.len()))
            } else {
                ("p", String::new())
            };
            out.push(format!("{kind} {:04o} {}:{} {rel} {extra}", m.mode() & 0o7777, m.uid(), m.gid()));
            if ft.is_dir() {
                walk(root, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

fn sandbox_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("diff").join(name)
}

fn reset_sandbox(dir: &Path) {
    if dir.exists() {
        // Restore permissions a test may have removed, then delete.
        let _ = Command::new("chmod").arg("-R").arg("u+rwx").arg(dir).status();
        std::fs::remove_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(dir).unwrap();
}

fn command(bin: &Path, case: &Case, root: Option<&Path>) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg0(case.applet);
    let root_s = root.map(|r| r.display().to_string()).unwrap_or_default();
    cmd.args(case.args.iter().map(|a| a.replace("$ROOT", &root_s)));
    cmd.env_clear().env("PATH", "/usr/bin:/bin");
    for (k, v) in &case.env {
        cmd.env(k, v.replace("$ROOT", &root_s));
    }
    cmd.current_dir(root.unwrap_or(Path::new("/")));
    cmd
}

/// Run on a pty: returns (merged output, status).
fn run_pty(mut cmd: Command) -> (Vec<u8>, std::process::ExitStatus) {
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    let (mut master, mut slave) = (0, 0);
    let ws = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: openpty writes two fds; the name/termios pointers may be null.
    let r = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws) };
    assert_eq!(r, 0, "openpty failed");
    // SAFETY: openpty returned these fds and nothing else owns them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    cmd.stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    let mut child = cmd.spawn().unwrap();
    drop(cmd); // closes our copies of the slave
    let mut out = Vec::new();
    let mut f = std::fs::File::from(master);
    let mut buf = [0u8; 4096];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break, // EIO once the child side is closed
        }
    }
    (out, child.wait().unwrap())
}

fn run(bin: &Path, case: &Case, name: &str) -> Outcome {
    let root = case.setup.map(|setup| {
        let dir = sandbox_path(name);
        reset_sandbox(&dir);
        setup(&dir);
        dir
    });
    let cmd = command(bin, case, root.as_deref());
    let (stdout, stderr, status) = if case.pty {
        let (out, st) = run_pty(cmd);
        (out, Vec::new(), st)
    } else {
        let mut cmd = cmd;
        let o = cmd.stdin(Stdio::null()).output().unwrap();
        (o.stdout, o.stderr, o.status)
    };
    let tree = root.as_deref().map(snapshot).unwrap_or_default();
    Outcome { stdout, stderr, status: status_string(status), tree }
}

fn show(b: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(b))
}

/// Run every case against both implementations; panic with a report of
/// all mismatches.
pub fn check_all(group: &str, cases: &[Case]) {
    let Some(refdir) = reference_dir() else { return };
    let mut failures = String::new();
    for (i, case) in cases.iter().enumerate() {
        let name = format!("{group}-{i}");
        let (cbin, rbin) = match &case.bins {
            Some((c, r)) => (c.clone(), r.clone()),
            None => (reference_bin(&refdir, case.applet), PathBuf::from(OURS)),
        };
        let want = run(&cbin, case, &name);
        let got = run(&rbin, case, &name);
        if want != got {
            let _ = writeln!(failures, "\n=== {} ===", case.describe());
            if want.stdout != got.stdout {
                let _ = writeln!(failures, "stdout:\n  C:    {}\n  Rust: {}", show(&want.stdout), show(&got.stdout));
            }
            if want.stderr != got.stderr {
                let _ = writeln!(failures, "stderr:\n  C:    {}\n  Rust: {}", show(&want.stderr), show(&got.stderr));
            }
            if want.status != got.status {
                let _ = writeln!(failures, "status: C {} / Rust {}", want.status, got.status);
            }
            if want.tree != got.tree {
                let _ = writeln!(failures, "tree:\n  C:    {:?}\n  Rust: {:?}", want.tree, got.tree);
            }
        }
    }
    assert!(failures.is_empty(), "{group}: behaviour differs from C reference:{failures}");
    eprintln!("{group}: {} cases identical to the C reference", cases.len());
}

/// The current user's name, for chown cases.
pub fn whoami() -> String {
    let o = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

pub fn osstr(s: &str) -> OsString {
    OsString::from_vec(s.as_bytes().to_vec())
}

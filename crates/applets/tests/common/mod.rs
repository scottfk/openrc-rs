//! Differential test harness: run the C reference build of OpenRC and
//! openrc-rs on the same input and require identical behaviour.
//!
//! Compared: stdout, stderr, exit status, and (for cases with a sandbox)
//! the resulting filesystem tree.  Both run with the same `argv[0]`, a
//! clean environment, and the same sandbox path, one after the other.
//!
//! Cases without a sandbox must not create anything: they also run as
//! root on the VM test bed.  For "missing" paths use ones that cannot be
//! created even by root, e.g. under /proc (`/proc/openrc-rs-nonexistent`).
//!
//! The reference is built by `tools/build-reference.sh` (`make reference`).
//! Without it these tests are skipped, unless `OPENRC_REF_REQUIRED=1`.

#![allow(dead_code)]

pub mod system;

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

/// Where the reference build puts an applet: `src/<dir>/<applet>`, where
/// multi-name programs share their source directory's name.
fn reference_bin(refdir: &Path, applet: &str) -> PathBuf {
    const SHARED: &[(&str, &[&str])] = &[
        (
            "mark_service",
            &[
                "mark_service_crashed",
                "mark_service_failed",
                "mark_service_hotplugged",
                "mark_service_inactive",
                "mark_service_started",
                "mark_service_starting",
                "mark_service_stopped",
                "mark_service_stopping",
                "mark_service_wasinactive",
            ],
        ),
        ("value", &["service_get_value", "service_set_value", "service_export", "get_options", "save_options"]),
        ("mountinfo", &["do_unmount"]),
    ];
    let dir = SHARED
        .iter()
        .find(|(_, names)| names.contains(&applet))
        .map(|(d, _)| *d)
        .unwrap_or_else(|| if refdir.join(applet).join(applet).exists() { applet } else { "einfo" });
    refdir.join(dir).join(applet)
}

/// One differential test case.
#[derive(Clone)]
pub struct Case {
    pub applet: &'static str,
    pub args: Vec<String>,
    /// Values are bytes so non-UTF-8 environments can be tested.
    pub env: Vec<(String, Vec<u8>)>,
    /// Run with stdin/stdout/stderr on a pseudo-terminal (output merged).
    pub pty: bool,
    /// Capture syslog messages on a private /dev/log (root only, and only
    /// when no syslog daemon owns /dev/log).
    pub syslog: bool,
    /// Create a sandbox directory, populated by this function; `$ROOT` in
    /// args is replaced by its path, and its final state is compared.
    pub setup: Option<fn(&Path)>,
    /// Explicit (C, Rust) programs instead of the applet's binaries.
    pub bins: Option<(PathBuf, PathBuf)>,
    /// Run the *reference C program* both times: once on the C libraries,
    /// once with LD_LIBRARY_PATH pointing at our librc.so.1/libeinfo.so.1
    /// (the reference binaries use RUNPATH, which that overrides).
    pub ourlibs: bool,
    /// Run in a private user + mount namespace (see [`Ns`]).
    pub ns: Option<Ns>,
    /// Run as PID 1 of a fresh user + PID namespace (with its own /proc),
    /// after this bash prelude has run there.  For programs that signal
    /// "every process" (kill_all): nothing outside the namespace is visible.
    pub pidns: Option<&'static str>,
}

/// Namespace setup for a case.  The child becomes root of a new user
/// namespace with its own mount namespace, so fixtures can be mounted
/// without privileges and without affecting the host.
#[derive(Clone, Debug, Default)]
pub struct Ns {
    /// Contents for a fixture bind-mounted over /etc/fstab (`$ROOT` expanded).
    pub fstab: Option<String>,
    /// tmpfs mounts to create, in order (`$ROOT` expanded); each mount
    /// point is created (mkdir) just before mounting.
    pub tmpfs: Vec<String>,
    /// Bind mounts `(source, target)` (`$ROOT` expanded), after the tmpfs
    /// mounts: e.g. fixture directories over /etc/init.d or /run/openrc.
    pub binds: Vec<(String, String)>,
}

impl Case {
    pub fn new(applet: &'static str, args: &[&str]) -> Case {
        Case {
            applet,
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            pty: false,
            syslog: false,
            setup: None,
            bins: None,
            ourlibs: false,
            ns: None,
            pidns: None,
        }
    }
    pub fn env(self, k: &str, v: &str) -> Case {
        self.env_bytes(k, v.as_bytes())
    }
    pub fn env_bytes(mut self, k: &str, v: &[u8]) -> Case {
        self.env.push((k.into(), v.to_vec()));
        self
    }
    pub fn pidns(mut self, prelude: &'static str) -> Case {
        self.pidns = Some(prelude);
        self
    }
    /// A reference C program, compared on C vs our libraries.
    pub fn on_our_libs(applet: &'static str, args: &[&str]) -> Case {
        let mut c = Case::new(applet, args);
        c.ourlibs = true;
        c
    }
    pub fn ns(mut self, ns: Ns) -> Case {
        self.ns = Some(ns);
        self
    }
    pub fn syslog(mut self) -> Case {
        self.syslog = true;
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
        let env: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={:?}", String::from_utf8_lossy(v))).collect();
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
    syslog: Vec<String>,
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
    let root_s = root.map(|r| r.display().to_string()).unwrap_or_default();
    let mut cmd = match case.pidns {
        None => {
            let mut c = Command::new(bin);
            c.arg0(case.applet);
            c
        }
        Some(prelude) => {
            // unshare forks, so bash is PID 1; it runs the prelude and then
            // execs the program (which becomes PID 1) under argv[0].
            let mut c = Command::new("/usr/bin/unshare");
            c.args(["--user", "--map-root-user", "--pid", "--fork", "--mount", "--mount-proc", "/bin/bash", "-c"]);
            c.arg(format!("{prelude}\nexec -a {} \"$0\" \"$@\"", case.applet));
            c.arg(bin);
            c
        }
    };
    cmd.args(case.args.iter().map(|a| a.replace("$ROOT", &root_s)));
    cmd.env_clear().env("PATH", "/usr/bin:/bin");
    for (k, v) in &case.env {
        cmd.env(k, osstr_bytes(&replace_bytes(v, b"$ROOT", root_s.as_bytes())));
    }
    cmd.current_dir(root.unwrap_or(Path::new("/")));
    if let Some(ns) = &case.ns {
        enter_ns_before_exec(&mut cmd, ns, &root_s, root);
    }
    cmd
}

/// Arrange for the child to enter a fresh user+mount namespace and set up
/// `ns` before exec.  Everything the child needs is allocated here: after
/// fork (in a multi-threaded test process) it may only make raw syscalls.
fn enter_ns_before_exec(cmd: &mut Command, ns: &Ns, root_s: &str, root: Option<&Path>) {
    use std::ffi::CString;
    // SAFETY: getuid/getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let uid_map = format!("0 {uid} 1");
    let gid_map = format!("0 {gid} 1");
    let fstab_src = ns.fstab.as_ref().map(|contents| {
        let dir = root.expect("fstab fixture needs a sandbox");
        let f = dir.with_extension("fstab");
        std::fs::write(&f, contents.replace("$ROOT", root_s)).unwrap();
        CString::new(f.display().to_string()).unwrap()
    });
    let tmpfs: Vec<CString> = ns.tmpfs.iter().map(|t| CString::new(t.replace("$ROOT", root_s)).unwrap()).collect();
    let binds: Vec<(CString, CString)> = ns
        .binds
        .iter()
        .map(|(a, b)| {
            (CString::new(a.replace("$ROOT", root_s)).unwrap(), CString::new(b.replace("$ROOT", root_s)).unwrap())
        })
        .collect();

    fn write_file(path: &std::ffi::CStr, data: &[u8]) -> std::io::Result<()> {
        // SAFETY: valid C string and buffer.
        unsafe {
            let fd = libc::open(path.as_ptr(), libc::O_WRONLY);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let n = libc::write(fd, data.as_ptr().cast(), data.len());
            libc::close(fd);
            if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
        }
    }
    fn check(r: libc::c_int) -> std::io::Result<()> {
        if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }

    // SAFETY: the closure only makes async-signal-safe syscalls on data
    // prepared above.
    unsafe {
        cmd.pre_exec(move || {
            check(libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS))?;
            write_file(c"/proc/self/setgroups", b"deny")?;
            write_file(c"/proc/self/uid_map", uid_map.as_bytes())?;
            write_file(c"/proc/self/gid_map", gid_map.as_bytes())?;
            check(libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ))?;
            if let Some(src) = &fstab_src {
                check(libc::mount(
                    src.as_ptr(),
                    c"/etc/fstab".as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                ))?;
            }
            for t in &tmpfs {
                libc::mkdir(t.as_ptr(), 0o755);
                check(libc::mount(c"tmpfs".as_ptr(), t.as_ptr(), c"tmpfs".as_ptr(), 0, c"size=1m".as_ptr().cast()))?;
            }
            for (src, dst) in &binds {
                check(libc::mount(
                    src.as_ptr(),
                    dst.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND | libc::MS_REC,
                    std::ptr::null(),
                ))?;
            }
            Ok(())
        });
    }
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

fn run(bin: &Path, case: &Case, name: &str, ourlibs: bool) -> Outcome {
    let root = case.setup.map(|setup| {
        let dir = sandbox_path(name);
        reset_sandbox(&dir);
        setup(&dir);
        dir
    });
    let log = case.syslog.then(SyslogCapture::start);
    let mut cmd = command(bin, case, root.as_deref());
    if ourlibs {
        cmd.env("LD_LIBRARY_PATH", our_libdir());
    }
    let (stdout, stderr, status) = if case.pty {
        let (out, st) = run_pty(cmd);
        (out, Vec::new(), st)
    } else {
        let mut cmd = cmd;
        let o = cmd.stdin(Stdio::null()).output().unwrap();
        (o.stdout, o.stderr, o.status)
    };
    let tree = root.as_deref().map(snapshot).unwrap_or_default();
    let syslog = log.map(SyslogCapture::finish).unwrap_or_default();
    Outcome { stdout, stderr, status: status_string(status), tree, syslog }
}

/// As root (the VM), the names in `/`, to catch cases that write outside
/// their sandbox; empty (unchecked) otherwise.
fn root_entries() -> Vec<std::ffi::OsString> {
    if !is_root() {
        return Vec::new();
    }
    let mut v: Vec<_> = std::fs::read_dir("/").unwrap().flatten().map(|e| e.file_name()).collect();
    v.sort();
    v
}

/// Our C ABI libraries (`make capi`).
pub fn our_libdir() -> PathBuf {
    repo_root().join("target/capi")
}

/// Can C programs be run on our libraries (has `make capi` been run)?
/// Panics instead of skipping if OPENRC_REF_REQUIRED is set.
pub fn our_libs_built() -> bool {
    let ok = our_libdir().join("librc.so.1").exists() && our_libdir().join("libeinfo.so.1").exists();
    if !ok {
        if std::env::var_os("OPENRC_REF_REQUIRED").is_some() {
            panic!("target/capi libraries missing; run `make capi`");
        }
        eprintln!("SKIPPED: target/capi libraries missing; run `make capi`");
    }
    ok
}

/// tests/capi/rc-probe.c compiled against the reference librc (RUNPATH);
/// run it with `ourlibs` to compare libraries.  None (skip) if the
/// reference or our libraries are missing.
pub fn rc_probe() -> Option<PathBuf> {
    let refdir = reference_dir()?;
    if !our_libs_built() {
        return None;
    }
    // Compiled once per test process: parallel tests would otherwise
    // rewrite the binary while another one runs it (ETXTBSY).
    static PROBE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    Some(
        PROBE
            .get_or_init(|| {
                let root = repo_root();
                let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
                let out = dir.join("rc-probe");
                let tmp = dir.join(format!("rc-probe.{}", std::process::id()));
                let libdir = refdir.join("librc");
                let st = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()))
                    .arg("-o")
                    .arg(&tmp)
                    .arg(root.join("tests/capi/rc-probe.c"))
                    .arg(format!("-I{}", root.join("crates/capi-rc/include").display()))
                    .arg(format!("-L{}", libdir.display()))
                    .arg(format!("-Wl,-rpath,{}", libdir.display()))
                    .arg("-lrc")
                    .status()
                    .unwrap();
                assert!(st.success(), "compiling rc-probe");
                std::fs::rename(&tmp, &out).unwrap();
                out
            })
            .clone(),
    )
}

/// Is this test process root?
pub fn is_root() -> bool {
    // SAFETY: geteuid cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// A throwaway syslog: binds /dev/log for the duration of one run.
struct SyslogCapture {
    sock: std::os::unix::net::UnixDatagram,
}

const DEV_LOG: &str = "/dev/log";

impl SyslogCapture {
    fn start() -> SyslogCapture {
        assert!(is_root(), "syslog capture needs root");
        let _ = std::fs::remove_file(DEV_LOG);
        let sock = std::os::unix::net::UnixDatagram::bind(DEV_LOG).expect("bind /dev/log");
        sock.set_nonblocking(true).unwrap();
        SyslogCapture { sock }
    }

    /// Collect what arrived, normalised: glibc sends
    /// `<PRI>Mmm dd hh:mm:ss ident[pid]: msg`; drop the time and pid.
    fn finish(self) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 65536];
        while let Ok(n) = self.sock.recv(&mut buf) {
            let m = String::from_utf8_lossy(&buf[..n]).into_owned();
            let norm = match (m.find('>'), m.find("]: "), m.find('[')) {
                (Some(gt), Some(end), Some(lb)) if gt + 16 <= lb => {
                    format!("{} {}[PID]{}", &m[..=gt], &m[gt + 16..lb], &m[end + 1..])
                }
                _ => m,
            };
            out.push(norm);
        }
        drop(self.sock);
        let _ = std::fs::remove_file(DEV_LOG);
        out
    }
}

/// Can syslog be captured here (root, and no daemon owns /dev/log)?
pub fn syslog_capturable() -> bool {
    if !is_root() {
        return false;
    }
    match std::fs::symlink_metadata(DEV_LOG) {
        Err(_) => true,
        // A stale socket is fine; a live daemon's is not.
        Ok(_) => std::os::unix::net::UnixDatagram::unbound().unwrap().connect(DEV_LOG).is_err(),
    }
}

fn show(b: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(b))
}

/// Run every case against both implementations; panic with a report of
/// all mismatches.  With `OPENRC_DIFF_DUMP=<dir>`, each case's command and
/// C output are also saved there.
pub fn check_all(group: &str, cases: &[Case]) {
    let Some(refdir) = reference_dir() else { return };
    let mut failures = String::new();
    for (i, case) in cases.iter().enumerate() {
        let name = format!("{group}-{i}");
        let (cbin, rbin) = match &case.bins {
            Some((c, r)) => (c.clone(), r.clone()),
            None if case.ourlibs => (reference_bin(&refdir, case.applet), reference_bin(&refdir, case.applet)),
            None => (reference_bin(&refdir, case.applet), PathBuf::from(OURS)),
        };
        let before = root_entries();
        let t0 = std::time::Instant::now();
        let want = run(&cbin, case, &name, false);
        let got = run(&rbin, case, &name, case.ourlibs);
        if t0.elapsed() > std::time::Duration::from_secs(2) {
            eprintln!("{group}: slow case ({:.1}s for both runs): {}", t0.elapsed().as_secs_f64(), case.describe());
        }
        if let Some(dir) = std::env::var_os("OPENRC_DIFF_DUMP") {
            // For reading what a case actually does: the C run's output.
            let dir = PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(format!("{name}.cmd")), case.describe());
            let _ = std::fs::write(dir.join(format!("{name}.out")), &want.stdout);
            let _ = std::fs::write(dir.join(format!("{name}.err")), &want.stderr);
        }
        let after = root_entries();
        assert_eq!(before, after, "{}: case changed / (tests must stay in their sandbox): {}", group, case.describe());
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
            if want.syslog != got.syslog {
                let _ = writeln!(failures, "syslog:\n  C:    {:?}\n  Rust: {:?}", want.syslog, got.syslog);
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
    osstr_bytes(s.as_bytes())
}

pub fn osstr_bytes(b: &[u8]) -> OsString {
    OsString::from_vec(b.to_vec())
}

/// Replace every `from` in `hay` with `to`.
fn replace_bytes(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

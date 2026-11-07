//! The dependency tree (`librc-depend.c`): generating the cache
//! (`rc_deptree_update`), loading it, the staleness check, and resolving
//! dependencies and start order.
//!
//! The data structures mirror C's: a tree is an ordered list of services,
//! each with an ordered list of dependency types, each an ordered list of
//! names.  Lookups take the first match, lists may hold duplicates
//! (`rc_stringlist_add`) or not (`addu`), and deletion removes the first
//! match only, exactly as with `RC_STRINGLIST`.  Start order and
//! tie-breaking fall out of that ordering, so the code follows C's loops
//! step by step rather than using maps or sets.

use std::io::Write;

use openrc_compat::sys::{self, libc};
use openrc_compat::{bcat, env};

use crate::state::{self, flags};
use crate::svcdir::{self, RcDir};

/// `RC_DEP_*` options.
pub mod options {
    pub const TRACE: i32 = 1 << 0;
    pub const STRICT: i32 = 1 << 1;
    pub const START: i32 = 1 << 2;
    pub const STOP: i32 = 1 << 3;
}
use options::{START, STOP, STRICT, TRACE};

use crate::state::{RC_LEVEL_SHUTDOWN, RC_LEVEL_SYSINIT};

/// One dependency type of a service (`RC_DEPTYPE`): `ineed`, `iuse`, ...
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepType {
    pub ty: Vec<u8>,
    pub services: Vec<Vec<u8>>,
}

/// A service and its dependencies (`RC_DEPINFO`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepInfo {
    pub service: Vec<u8>,
    pub depends: Vec<DepType>,
}

impl DepInfo {
    /// `get_deptype()`: the first type named `ty`.
    pub fn get(&self, ty: &[u8]) -> Option<&DepType> {
        self.depends.iter().find(|d| d.ty == ty)
    }

    fn get_idx(&self, ty: &[u8]) -> Option<usize> {
        self.depends.iter().position(|d| d.ty == ty)
    }

    /// `get_deptype()`, or `make_deptype()` (appended) if there is none.
    fn get_or_make(&mut self, ty: &[u8]) -> usize {
        match self.get_idx(ty) {
            Some(i) => i,
            None => self.make(ty),
        }
    }

    fn make(&mut self, ty: &[u8]) -> usize {
        self.depends.push(DepType { ty: ty.to_vec(), services: Vec::new() });
        self.depends.len() - 1
    }
}

/// `RC_DEPTREE`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DepTree {
    pub infos: Vec<DepInfo>,
}

impl DepTree {
    /// `get_depinfo()`: the first entry for `service`.
    pub fn get(&self, service: &[u8]) -> Option<&DepInfo> {
        self.infos.iter().find(|d| d.service == service)
    }

    fn get_idx(&self, service: &[u8]) -> Option<usize> {
        self.infos.iter().position(|d| d.service == service)
    }

    fn make(&mut self, service: &[u8]) -> usize {
        self.infos.push(DepInfo { service: service.to_vec(), depends: Vec::new() });
        self.infos.len() - 1
    }
}

/// `rc_stringlist_addu()`: append unless present.  False if it was.
fn addu(list: &mut Vec<Vec<u8>>, value: &[u8]) -> bool {
    if list.iter().any(|s| s == value) {
        return false;
    }
    list.push(value.to_vec());
    true
}

/// `rc_stringlist_delete()`: remove the first match.
fn delete(list: &mut Vec<Vec<u8>>, value: &[u8]) -> bool {
    match list.iter().position(|s| s == value) {
        Some(i) => {
            list.remove(i);
            true
        }
        None => false,
    }
}

/// `strsep(&p, delim)` on a byte slice.
fn strsep<'a>(p: &mut Option<&'a [u8]>, delim: &[u8]) -> Option<&'a [u8]> {
    let s = p.take()?;
    match s.iter().position(|b| delim.contains(b)) {
        Some(i) => {
            *p = Some(&s[i + 1..]);
            Some(&s[..i])
        }
        None => Some(s),
    }
}

/// The lines `xgetline` returns: split on `\n` (stripped), the last line
/// kept even without one.  C then treats each as a C string, so a line
/// ends at its first NUL.
fn c_lines(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let body = data.strip_suffix(b"\n").unwrap_or(data);
    let lines = if data.is_empty() { None } else { Some(body.split(|&b| b == b'\n')) };
    lines.into_iter().flatten().map(|l| match l.iter().position(|&b| b == 0) {
        Some(i) => &l[..i],
        None => l,
    })
}

/// `get_shell_value()`: strip one leading quote, then a trailing newline
/// and quote.  None if nothing is left.
fn get_shell_value(s: &[u8]) -> Option<&[u8]> {
    // C walks back from the end; on "'" alone (or "'\n") it steps onto
    // the leading quote itself, which ends up stripped either way.
    let mut v = s.strip_prefix(b"'").unwrap_or(s);
    if v.is_empty() {
        return None;
    }
    if let Some(r) = v.strip_suffix(b"\n") {
        v = r;
    }
    if let Some(r) = v.strip_suffix(b"'") {
        v = r;
    }
    (!v.is_empty()).then_some(v)
}

/// `deptree_load_file()`: parse a `deptree` cache.
pub fn parse(data: &[u8]) -> DepTree {
    let mut tree = DepTree::default();
    let mut depinfo: Option<usize> = None;
    let mut deptype: Option<usize> = None;
    for line in c_lines(data) {
        let mut p = Some(line);
        if strsep(&mut p, b"_") != Some(&b"depinfo"[..]) {
            continue;
        }
        match strsep(&mut p, b"_") {
            Some(e) if openrc_compat::num::scanf_int(e).is_some() => {}
            _ => continue,
        }
        let Some(ty) = strsep(&mut p, b"_=") else { continue };
        if ty == b"service" {
            // Sanity
            let Some(e) = p.and_then(get_shell_value) else { continue };
            depinfo = Some(tree.make(e));
            deptype = None;
            continue;
        }
        match strsep(&mut p, b"=") {
            Some(e) if openrc_compat::num::scanf_int(e).is_some() => {}
            _ => continue,
        }
        // Sanity
        let Some(e) = p.and_then(get_shell_value) else { continue };
        let Some(di) = depinfo else { continue };
        let info = &mut tree.infos[di];
        // QUIRK: a type that isn't contiguous with its earlier lines gets
        // a second entry, not appended to the first.
        let dt = match deptype {
            Some(d) if info.depends[d].ty == ty => d,
            _ => info.make(ty),
        };
        deptype = Some(dt);
        info.depends[dt].services.push(e.to_vec());
    }
    tree
}

fn load_at(dirfd: i32, path: &[u8]) -> Option<DepTree> {
    use std::io::Read;
    let fd = sys::at::openat(dirfd, path, libc::O_RDONLY, 0o666).ok()?;
    let mut data = Vec::new();
    // As with getline, a read error just ends the input.
    let _ = std::fs::File::from(fd).read_to_end(&mut data);
    Some(parse(&data))
}

/// `rc_deptree_load()`: `deptree` in the service directory.
pub fn rc_deptree_load() -> Option<DepTree> {
    load_at(svcdir::rc_dirfd_raw(RcDir::Svcdir), b"deptree")
}

/// `rc_deptree_load_file()`.
pub fn rc_deptree_load_file(path: &[u8]) -> Option<DepTree> {
    load_at(libc::AT_FDCWD, path)
}

/// `rc_deptree_depend()`: a copy of one dependency list, or Err(ENOENT)
/// (with an empty list) if the service or type isn't there.
pub fn rc_deptree_depend(tree: &DepTree, service: &[u8], ty: &[u8]) -> Result<Vec<Vec<u8>>, i32> {
    match tree.get(service).and_then(|d| d.get(ty)) {
        Some(dt) => Ok(dt.services.clone()),
        None => Err(libc::ENOENT),
    }
}

/// The resolver state C keeps in statics: the boot runlevel, read from
/// `$RC_BOOTLEVEL` by `rc_deptree_depends`/`rc_deptree_order`.
struct Resolver<'a> {
    tree: &'a DepTree,
    bootlevel: Vec<u8>,
}

fn in_runlevel(service: &[u8], runlevel: Option<&[u8]>) -> bool {
    // Deviation: C formats a NULL runlevel as "(null)" here (glibc's %s)
    // and crashes soon after; we only get here when a caller passed NULL.
    state::rc_service_in_runlevel(service, runlevel.unwrap_or(b"(null)"))
}

impl Resolver<'_> {
    fn new(tree: &DepTree) -> Resolver<'_> {
        let bootlevel = env::get_bytes(b"RC_BOOTLEVEL").unwrap_or_else(|| state::RC_LEVEL_BOOT.to_vec());
        Resolver { tree, bootlevel }
    }

    /// For update's phase 5, which never consults the bootlevel.
    fn plain(tree: &DepTree) -> Resolver<'_> {
        Resolver { tree, bootlevel: Vec::new() }
    }

    fn valid_service(&self, runlevel: Option<&[u8]>, service: &[u8], ty: &[u8]) -> bool {
        let Some(rl) = runlevel else { return true };
        if matches!(ty, b"ineed" | b"needsme" | b"iwant" | b"wantsme") {
            return true;
        }
        if state::rc_service_in_runlevel(service, rl) {
            return true;
        }
        if rl == RC_LEVEL_SYSINIT {
            return false;
        }
        if rl == RC_LEVEL_SHUTDOWN && ty == b"iafter" {
            return false;
        }
        if rl != self.bootlevel.as_slice() && state::rc_service_in_runlevel(service, &self.bootlevel) {
            return true;
        }
        let st = state::rc_service_state(service);
        st & flags::HOTPLUGGED != 0 || st & flags::STARTED != 0
    }

    /// `get_provided1()`: append the providers in `dt` that match.
    fn get_provided1(
        &self,
        runlevel: Option<&[u8]>,
        providers: &mut Vec<Vec<u8>>,
        dt: &DepType,
        level: Option<&[u8]>,
        hotplugged: bool,
        want: i32,
    ) -> bool {
        let mut retval = false;
        for svc in &dt.services {
            let st = state::rc_service_state(svc);
            let mut ok = true;
            if let Some(level) = level {
                ok = state::rc_service_in_runlevel(svc, level);
            } else if hotplugged {
                ok = st & flags::HOTPLUGGED != 0
                    && !in_runlevel(svc, runlevel)
                    && !state::rc_service_in_runlevel(svc, &self.bootlevel);
            }
            if !ok {
                continue;
            }
            match want {
                flags::STARTED => ok = st & flags::STARTED != 0,
                flags::INACTIVE | flags::STARTING | flags::STOPPING => {
                    ok = st & (flags::STARTING | flags::STOPPING | flags::INACTIVE) != 0;
                }
                _ => {}
            }
            if !ok {
                continue;
            }
            retval = true;
            providers.push(svc.clone());
        }
        retval
    }

    /// `get_provided()`: which services provide the virtual `depinfo`,
    /// given what's running, starting, and in the runlevels.
    fn get_provided(&self, depinfo: &DepInfo, runlevel: Option<&[u8]>, options: i32) -> Vec<Vec<u8>> {
        let mut providers = Vec::new();
        let Some(dt) = depinfo.get(b"providedby") else { return providers };

        // Stopping: every provider counts.
        if options & STOP != 0 {
            return dt.services.clone();
        }

        // Strict or starting: only the runlevel and bootlevel (and
        // hotplugged, when starting).
        if options & STRICT != 0 || options & START != 0 {
            for s in &dt.services {
                if in_runlevel(s, runlevel)
                    || state::rc_service_in_runlevel(s, &self.bootlevel)
                    || (options & START != 0 && state::rc_service_state(s) & flags::HOTPLUGGED != 0)
                {
                    providers.push(s.clone());
                }
            }
            if !providers.is_empty() {
                return providers;
            }
        }

        // C's DO macro: more than one match means "already satisfied".
        let done = |p: Vec<Vec<u8>>| if p.len() > 1 { Vec::new() } else { p };
        let rl = runlevel.unwrap_or(b"(null)");
        let other_boot = rl != self.bootlevel.as_slice();
        let boot = Some(self.bootlevel.as_slice());

        // Anything running has to come first.
        if self.get_provided1(runlevel, &mut providers, dt, runlevel, false, flags::STARTED) {
            return done(providers);
        }
        if self.get_provided1(runlevel, &mut providers, dt, None, true, flags::STARTED) {
            return done(providers);
        }
        if other_boot && self.get_provided1(runlevel, &mut providers, dt, boot, false, flags::STARTED) {
            return done(providers);
        }
        if self.get_provided1(runlevel, &mut providers, dt, None, false, flags::STARTED) {
            return done(providers);
        }

        // Then starting (or stopping, or inactive) ones.
        if self.get_provided1(runlevel, &mut providers, dt, runlevel, false, flags::STARTING)
            || self.get_provided1(runlevel, &mut providers, dt, None, true, flags::STARTING)
            || (other_boot && self.get_provided1(runlevel, &mut providers, dt, boot, false, flags::STARTING))
            || self.get_provided1(runlevel, &mut providers, dt, None, false, flags::STARTING)
        {
            return providers;
        }

        // Nothing started, so the stopped ones.  ("Stopped" checks no state:
        // anything left by now is stopped.  Only the hotplugged check gets
        // the >1 rule.)
        if self.get_provided1(runlevel, &mut providers, dt, runlevel, false, flags::STOPPED) {
            return providers;
        }
        if self.get_provided1(runlevel, &mut providers, dt, None, true, flags::STOPPED) {
            return done(providers);
        }
        if other_boot && self.get_provided1(runlevel, &mut providers, dt, boot, false, flags::STOPPED) {
            return providers;
        }

        // Still nothing: the first provider.
        if let Some(first) = dt.services.first() {
            providers.push(first.clone());
        }
        providers
    }

    /// `visit_service()`: depth-first, appending to `sorted`.
    fn visit(
        &self,
        types: &[Vec<u8>],
        sorted: &mut Vec<Vec<u8>>,
        visited: &mut Vec<Vec<u8>>,
        depinfo: &DepInfo,
        runlevel: Option<&[u8]>,
        options: i32,
    ) {
        if visited.contains(&depinfo.service) {
            return;
        }
        visited.push(depinfo.service.clone());

        for ty in types {
            let Some(dt) = depinfo.get(ty) else { continue };
            for service in &dt.services {
                if options & TRACE == 0 || ty == b"iprovide" {
                    sorted.push(service.clone());
                    continue;
                }
                let Some(di) = self.tree.get(service) else { continue };
                let provided = self.get_provided(di, runlevel, options);
                if !provided.is_empty() {
                    for p in &provided {
                        if let Some(d) = self.tree.get(p)
                            && self.valid_service(runlevel, &d.service, ty)
                        {
                            self.visit(types, sorted, visited, d, runlevel, options | TRACE);
                        }
                    }
                } else if self.valid_service(runlevel, service, ty) {
                    self.visit(types, sorted, visited, di, runlevel, options | TRACE);
                }
            }
        }

        // Now visit the stuff we provide for.
        if options & TRACE != 0
            && let Some(dt) = depinfo.get(b"iprovide")
        {
            for service in &dt.services {
                let Some(di) = self.tree.get(service) else { continue };
                let provided = self.get_provided(di, runlevel, options);
                if provided.contains(&depinfo.service) {
                    self.visit(types, sorted, visited, di, runlevel, options | TRACE);
                }
            }
        }

        // Add ourselves, unless we're the calling service or provided by
        // something.
        let svcname = env::get_bytes(b"RC_SVCNAME");
        if svcname.as_deref() != Some(depinfo.service.as_slice()) && depinfo.get(b"providedby").is_none() {
            sorted.push(depinfo.service.clone());
        }
    }
}

/// `rc_deptree_depends()`: the services `services` depend on through
/// `types`, in start order.  The bool is whether a service wasn't in the
/// tree; as in C, errno is set to ENOENT right then and the walk carries
/// on.
pub fn rc_deptree_depends(
    tree: &DepTree,
    types: Option<&[Vec<u8>]>,
    services: &[Vec<u8>],
    runlevel: Option<&[u8]>,
    options: i32,
) -> (Vec<Vec<u8>>, bool) {
    let r = Resolver::new(tree);
    let mut sorted = Vec::new();
    let mut visited = Vec::new();
    let mut enoent = false;
    for s in services {
        let Some(di) = tree.get(s) else {
            enoent = true;
            sys::set_errno(libc::ENOENT);
            continue;
        };
        if let Some(types) = types {
            r.visit(types, &mut sorted, &mut visited, di, runlevel, options);
        }
    }
    (sorted, enoent)
}

/// `rc_deptree_order()`: everything to start for `runlevel` (or, for
/// single and shutdown, everything running), in order.
pub fn rc_deptree_order(tree: &DepTree, runlevel: &[u8], options: i32) -> Vec<Vec<u8>> {
    let bootlevel = env::get_bytes(b"RC_BOOTLEVEL").unwrap_or_else(|| state::RC_LEVEL_BOOT.to_vec());
    let mut list;
    if runlevel == state::RC_LEVEL_SINGLE || runlevel == RC_LEVEL_SHUTDOWN {
        list = state::rc_services_in_state(flags::STARTED);
        list.extend(state::rc_services_in_state(flags::INACTIVE));
        list.extend(state::rc_services_in_state(flags::STARTING));
    } else {
        list = state::rc_services_in_runlevel(Some(RC_LEVEL_SYSINIT));
        if runlevel != RC_LEVEL_SYSINIT {
            list.extend(state::rc_services_in_runlevel(Some(runlevel)));
            list.extend(state::rc_services_in_state(flags::HOTPLUGGED));
            if runlevel != bootlevel.as_slice() {
                list.extend(state::rc_services_in_runlevel(Some(&bootlevel)));
            }
        }
    }
    let types: Vec<Vec<u8>> = [&b"ineed"[..], b"iuse", b"iwant", b"iafter"].iter().map(|t| t.to_vec()).collect();
    rc_deptree_depends(tree, Some(&types), &list, Some(runlevel), STRICT | TRACE | options).0
}

/// What `rc_deptree_update_needed()` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNeeded {
    pub needed: bool,
    /// The newest mtime seen, which C stores only when an update is needed.
    pub newest: i64,
    /// The last file found newer than the cache (its name relative to its
    /// directory).
    pub file: Option<Vec<u8>>,
}

/// `rc_deptree_update_needed()`: is anything the cache depends on newer
/// than it (or is there no cache)?
pub fn rc_deptree_update_needed() -> UpdateNeeded {
    let dir = svcdir::rc_svcdir();
    if let Err(e) = sys::mkdir(&dir, 0o755)
        && e.0 != libc::EEXIST
    {
        let _ = std::io::stderr().write_all(&bcat!(b"mkdir '", &dir, b"': ", &e.strerror(), b"\n"));
    }

    svcdir::clear_dirfds(); // clear our cached dirfds as a sanity check

    let mut newer = false;
    let mut mtime = match sys::at::fstatat(svcdir::rc_dirfd_raw(RcDir::Svcdir), b"deptree", 0) {
        Ok(st) => st.mtime,
        Err(_) => {
            // No cache: still scan (clock skew), to report the newest time.
            newer = true;
            // SAFETY: time(NULL) is always safe.
            unsafe { libc::time(std::ptr::null_mut()) }
        }
    };
    let mut file = None;
    let mut check = |fd: i32, target: &[u8], mtime: &mut i64| {
        if !crate::mtime::deep_check(fd, target, true, mtime, &mut file) {
            newer = true;
        }
    };

    for &fd in crate::dirs::rc_scriptdirfds() {
        check(fd, b"init.d", &mut mtime);
        check(fd, b"conf.d", &mut mtime);
    }
    let sysconf = svcdir::rc_dirfd_raw(RcDir::Sysconf);
    check(sysconf, b"rc.conf", &mut mtime);
    check(sysconf, b"rc.conf.d", &mut mtime);
    if crate::dirs::rc_is_user() {
        let usrconf = svcdir::rc_dirfd_raw(RcDir::Usrconf);
        check(usrconf, b"rc.conf", &mut mtime);
        check(usrconf, b"rc.conf.d", &mut mtime);
    }

    // Config files outside the script dirs that some scripts' depends
    // read.  QUIRK: depconfig is read with config_list(), which keeps only
    // lines containing '=', so plain paths (all of them, in practice) are
    // never checked.
    for s in crate::conf::config_list_raw(svcdir::rc_dirfd_raw(RcDir::Svcdir), b"depconfig") {
        check(libc::AT_FDCWD, &s, &mut mtime);
    }

    UpdateNeeded { needed: newer, newest: mtime, file }
}

/// `setup_environment()`: what gendepends.sh needs.
fn setup_environment() {
    let dirs = crate::dirs::rc_scriptdirs();
    env::set("RC_SCRIPTDIRS", &dirs.join(&b' '));
    if env::get_bytes(b"RC_LIBEXECDIR").is_none() {
        env::set("RC_LIBEXECDIR", crate::paths::LIBEXECDIR.as_bytes());
    }
    // SAFETY: utsname is plain data; uname fills it.
    unsafe {
        let mut uts: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut uts) == 0 {
            let name = std::ffi::CStr::from_ptr(uts.sysname.as_ptr());
            env::set("RC_UNAME", name.to_bytes());
        }
    }
}

/// `popen(cmd, "r")`, read to EOF, `pclose`.  None if popen fails.
fn popen_read(cmd: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let c = openrc_compat::bytes::to_cstring(cmd);
    // SAFETY: valid C strings; the FILE is closed with pclose below.
    let fp = unsafe { libc::popen(c.as_ptr(), c"r".as_ptr()) };
    if fp.is_null() {
        return None;
    }
    // Read the pipe's fd directly; nothing else touches the FILE before
    // pclose, which only waits for the child.
    // SAFETY: fp is a valid FILE; we borrow its fd and don't close it.
    let fd = unsafe { libc::fileno(fp) };
    // SAFETY: fd is open for the life of fp; ManuallyDrop keeps it open.
    let mut f = std::mem::ManuallyDrop::new(unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) });
    let mut data = Vec::new();
    let _ = f.read_to_end(&mut data);
    // SAFETY: fp came from popen.
    unsafe { libc::pclose(fp) };
    Some(data)
}

const DEPPAIRS: [(&[u8], &[u8]); 6] = [
    (b"ineed", b"needsme"),
    (b"iuse", b"usesme"),
    (b"iwant", b"wantsme"),
    (b"iafter", b"ibefore"),
    (b"ibefore", b"iafter"),
    (b"iprovide", b"providedby"),
];

/// Phase 1: parse gendepends.sh's output into a tree and the `config`
/// file list.
fn parse_gendepends(data: &[u8]) -> (DepTree, Vec<Vec<u8>>) {
    let mut tree = DepTree::default();
    let mut config = Vec::new();
    let mut depinfo: Option<usize> = None;
    let mut deptype: Option<usize> = None;
    for line in c_lines(data) {
        let mut depends = Some(line);
        let service = match strsep(&mut depends, b" ") {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let ty = strsep(&mut depends, b" ");
        let di = match depinfo {
            Some(d) if tree.infos[d].service == service => d,
            _ => {
                deptype = None;
                let d = tree.get_idx(service).unwrap_or_else(|| tree.make(service));
                depinfo = Some(d);
                d
            }
        };

        // We may not have any depends.
        let (Some(ty), Some(_)) = (ty, depends) else { continue };

        let info = &mut tree.infos[di];
        if ty != b"config" {
            deptype = match deptype {
                Some(d) if info.depends[d].ty == ty => Some(d),
                _ => Some(info.get_or_make(ty)),
            };
        }

        // Each depend individually, so runs of spaces are harmless.
        while let Some(depend) = strsep(&mut depends, b" ") {
            if depend.is_empty() {
                continue;
            }
            if ty == b"config" {
                addu(&mut config, depend);
                continue;
            }
            // Don't depend on ourself.
            if depend == service {
                continue;
            }
            // .sh files are not init scripts.
            if depend.len() > 2 && depend.ends_with(b".sh") {
                continue;
            }
            let dt = deptype.expect("set above for non-config types");
            // Remove our dependency if instructed.
            if let Some(rest) = depend.strip_prefix(b"!") {
                delete(&mut info.depends[dt].services, rest);
                continue;
            }
            info.depends[dt].services.push(depend.to_vec());

            // `after *; before local;` and `before *; after modules` must
            // both work: being before something drops it from after, and
            // vice versa.
            if ty == b"ibefore"
                && let Some(a) = info.get_idx(b"iafter")
            {
                delete(&mut info.depends[a].services, depend);
            }
            if matches!(ty, b"iafter" | b"ineed" | b"iwant" | b"iuse")
                && let Some(b) = info.get_idx(b"ibefore")
            {
                delete(&mut info.depends[b].services, depend);
            }
        }
    }
    (tree, config)
}

/// Phase 2: on a special system, drop services with a `-<sys>` or
/// `no<sys>` keyword, and every reference to them and what they provide.
fn remove_nosys(tree: &mut DepTree, sys: &str) {
    let lower = sys.to_ascii_lowercase();
    let nosys = bcat!(b"-", lower.as_bytes());
    let onosys = bcat!(b"no", lower.as_bytes());
    let mut i = 0;
    while i < tree.infos.len() {
        let Some(kw) = tree.infos[i].get(b"keyword").cloned() else {
            i += 1;
            continue;
        };
        let mut removed: Option<DepInfo> = None;
        for s in &kw.services {
            if *s != nosys && *s != onosys {
                continue;
            }
            // QUIRK: a second matching keyword repeats the removal, so
            // another duplicate of each name is deleted from every list.
            let gone = removed.get_or_insert_with(|| tree.infos.remove(i));
            let provide = gone.get(b"iprovide").cloned();
            for di in &mut tree.infos {
                let mut k = 0;
                while k < di.depends.len() {
                    let dt = &mut di.depends[k];
                    delete(&mut dt.services, &gone.service);
                    if let Some(p) = &provide {
                        for s2 in &p.services {
                            delete(&mut dt.services, s2);
                        }
                    }
                    if dt.services.is_empty() {
                        di.depends.remove(k);
                    } else {
                        k += 1;
                    }
                }
            }
        }
        if removed.is_none() {
            i += 1;
        }
    }
}

/// Phases 3-6 of `rc_deptree_update()`, on a parsed tree.  Returns the
/// messages C prints to stderr, in order.
fn resolve(tree: &mut DepTree, sys: Option<&str>) -> Vec<Vec<u8>> {
    let mut errs = Vec::new();
    if let Some(sys) = sys {
        remove_nosys(tree, sys);
    }

    // Phase 3: the virtual services, after the real ones.
    let mut providers = DepTree::default();
    for di in &tree.infos {
        if let Some(dt) = di.get(b"iprovide") {
            for s in &dt.services {
                if providers.get(s).is_none() {
                    providers.make(s);
                }
            }
        }
    }
    tree.infos.extend(providers.infos);

    // Phase 4: back-references.  (The lists can grow while we walk them,
    // as C's TAILQ walks would see, so index afresh each time.)
    for i in 0..tree.infos.len() {
        for (depend, addto) in DEPPAIRS {
            let Some(dt) = tree.infos[i].get_idx(depend) else { continue };
            let mut k = 0;
            while k < tree.infos[i].depends[dt].services.len() {
                let s = tree.infos[i].depends[dt].services[k].clone();
                k += 1;
                let Some(di) = tree.get_idx(&s) else {
                    if depend == b"ineed" {
                        errs.push(bcat!(
                            b"Service '",
                            &tree.infos[i].service,
                            b"' needs non existent service '",
                            &s,
                            b"'\n"
                        ));
                        let b = tree.infos[i].get_or_make(b"broken");
                        addu(&mut tree.infos[i].depends[b].services, &s);
                    }
                    continue;
                };
                let me = tree.infos[i].service.clone();
                let t = tree.infos[di].get_or_make(addto);
                addu(&mut tree.infos[di].depends[t].services, &me);
            }
        }
    }

    // Phase 5: remove before directives that conflict with the
    // service's own dependencies.
    let types: Vec<Vec<u8>> = [&b"ineed"[..], b"iwant", b"iuse", b"iafter"].iter().map(|t| t.to_vec()).collect();
    for i in 0..tree.infos.len() {
        if tree.infos[i].get(b"ibefore").is_none() {
            continue;
        }
        let mut sorted = Vec::new();
        Resolver::plain(tree).visit(&types, &mut sorted, &mut Vec::new(), &tree.infos[i], None, 0);
        let me = tree.infos[i].service.clone();
        let bt = tree.infos[i].get_idx(b"ibefore").expect("checked above");
        let mut k = 0;
        while k < tree.infos[i].depends[bt].services.len() {
            let s2 = tree.infos[i].depends[bt].services[k].clone();
            let mut found = false;
            for s3 in &sorted {
                let Some(di) = tree.get_idx(s3) else { continue };
                if s2 == *s3 {
                    if let Some(a) = tree.infos[di].get_idx(b"iafter") {
                        delete(&mut tree.infos[di].depends[a].services, &me);
                    }
                    found = true;
                    break;
                }
                let Some(p) = tree.infos[di].get(b"iprovide") else { continue };
                if p.services.contains(&s2) {
                    if let Some(v) = tree.get_idx(&s2)
                        && let Some(a) = tree.infos[v].get_idx(b"iafter")
                    {
                        delete(&mut tree.infos[v].depends[a].services, &me);
                    }
                    found = true;
                    break;
                }
            }
            if found {
                // Deletes the first match, at or before k; either way the
                // next entry is now at k.
                delete(&mut tree.infos[i].depends[bt].services, &s2);
            } else {
                k += 1;
            }
        }
    }

    // Phase 6: real and virtual services with the same name.
    let mut dupes = Vec::new();
    for di in &tree.infos {
        if !addu(&mut dupes, &di.service) {
            errs.push(bcat!(b"Error: ", &di.service, b" is the name of a real and virtual service.\n"));
        }
    }
    errs
}

/// Phase 7's file format.
pub fn serialize(tree: &DepTree) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, di) in tree.infos.iter().enumerate() {
        out.extend(bcat!(b"depinfo_", i.to_string().as_bytes(), b"_service='", &di.service, b"'\n"));
        for dt in &di.depends {
            for (k, s) in dt.services.iter().enumerate() {
                out.extend(bcat!(
                    b"depinfo_",
                    i.to_string().as_bytes(),
                    b"_",
                    &dt.ty,
                    b"_",
                    k.to_string().as_bytes(),
                    b"='",
                    s,
                    b"'\n"
                ));
            }
        }
    }
    out
}

fn eprint(msg: &[u8]) {
    let _ = std::io::stderr().write_all(msg);
}

/// `rc_deptree_update()`: run gendepends.sh, resolve, and write
/// `deptree` (and `depconfig`) to the service directory.
pub fn rc_deptree_update() -> bool {
    // Phase 1: source every init script and collect its dependencies.
    setup_environment();
    let gendep = bcat!(crate::paths::LIBEXECDIR.as_bytes(), b"/sh/gendepends.sh");
    let Some(out) = popen_read(&gendep) else { return false };
    let (mut tree, config) = parse_gendepends(&out);

    for e in resolve(&mut tree, crate::sys::rc_sys()) {
        eprint(&e);
    }

    // Phase 7: save to disk.
    let mut retval = true;
    let svcfd = svcdir::rc_dirfd_raw(RcDir::Svcdir);
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC;
    let report = |what: &[u8], e: sys::Errno| {
        eprint(&bcat!(b"fopen '", &svcdir::rc_svcdir(), b"/", what, b"': ", &e.strerror(), b"\n"));
    };
    match sys::at::openat(svcfd, b"deptree", flags, 0o666) {
        Ok(fd) => sys::write_all_fd(std::os::fd::AsFd::as_fd(&fd), &serialize(&tree)),
        Err(e) => {
            report(b"deptree", e);
            retval = false;
        }
    }

    // Save our external config files to disk.
    if config.is_empty() {
        let _ = sys::at::unlinkat(svcfd, b"depconfig", 0);
    } else {
        match sys::at::openat(svcfd, b"depconfig", flags, 0o666) {
            Ok(fd) => {
                let data: Vec<u8> = config.iter().flat_map(|s| bcat!(s, b"\n")).collect();
                sys::write_all_fd(std::os::fd::AsFd::as_fd(&fd), &data);
            }
            Err(e) => {
                report(b"depconfig", e);
                retval = false;
            }
        }
    }
    retval
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<Vec<u8>> {
        items.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    #[test]
    fn shell_value() {
        assert_eq!(get_shell_value(b"'abc'"), Some(&b"abc"[..]));
        assert_eq!(get_shell_value(b"'abc'\n"), Some(&b"abc"[..]));
        assert_eq!(get_shell_value(b"abc"), Some(&b"abc"[..]));
        assert_eq!(get_shell_value(b"'"), None);
        assert_eq!(get_shell_value(b"''"), None);
        assert_eq!(get_shell_value(b""), None);
    }

    #[test]
    fn roundtrip() {
        let data =
            b"alpha ineed beta  gamma\nalpha iuse x.sh !y y\nbeta iprovide virt\nbeta\ngamma config /etc/a /etc/a\n";
        let (mut tree, config) = parse_gendepends(data);
        assert_eq!(config, v(&["/etc/a"]));
        assert_eq!(tree.get(b"alpha").unwrap().get(b"ineed").unwrap().services, v(&["beta", "gamma"]));
        // "!y" before "y": nothing to delete yet, then y is added.
        assert_eq!(tree.get(b"alpha").unwrap().get(b"iuse").unwrap().services, v(&["y"]));
        let errs = resolve(&mut tree, None);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(tree.get(b"virt").unwrap().get(b"providedby").unwrap().services, v(&["beta"]));
        assert_eq!(parse(&serialize(&tree)), tree);
    }

    #[test]
    fn missing_need() {
        let (mut tree, _) = parse_gendepends(b"a ineed nope\n");
        let errs = resolve(&mut tree, None);
        assert_eq!(errs, v(&["Service 'a' needs non existent service 'nope'\n"]));
        assert_eq!(tree.get(b"a").unwrap().get(b"broken").unwrap().services, v(&["nope"]));
    }
}

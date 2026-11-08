//! Port of upstream's `src/shared/plugin.c`: load the plugins in
//! `RC_PLUGINDIR` and run their hook, each in a child process, at the
//! points openrc-run (and later openrc) reach.  A plugin can set our
//! environment by writing `NAME=value\0` records to `rc_environ_fd`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

use openrc_compat::dl::Library;
use openrc_compat::{bcat, env, exec, stdio, sys};
use openrc_einfo::eerror;

/// `RC_HOOK` values.
pub mod hook {
    pub const SERVICE_STOP_IN: i32 = 101;
    pub const SERVICE_STOP_NOW: i32 = 102;
    pub const SERVICE_STOP_DONE: i32 = 103;
    pub const SERVICE_STOP_OUT: i32 = 104;
    pub const SERVICE_START_IN: i32 = 105;
    pub const SERVICE_START_NOW: i32 = 106;
    pub const SERVICE_START_DONE: i32 = 107;
    pub const SERVICE_START_OUT: i32 = 108;
}

/// `rc_in_plugin`: set in a plugin's child process.
static IN_PLUGIN: AtomicBool = AtomicBool::new(false);
/// `plugins`: name and handle, in load order.
static PLUGINS: Mutex<Vec<(Vec<u8>, Library)>> = Mutex::new(Vec::new());

/// Are we a plugin's child process?
pub fn rc_in_plugin() -> bool {
    IN_PLUGIN.load(SeqCst)
}

fn plugins() -> std::sync::MutexGuard<'static, Vec<(Vec<u8>, Library)>> {
    PLUGINS.lock().unwrap_or_else(|e| e.into_inner())
}

/// `rc_plugin_load()`: dlopen everything in the plugin directory (in
/// readdir order, skipping dot files) that has an `rc_plugin_hook`.
pub fn rc_plugin_load() {
    if rc_in_plugin() {
        return;
    }
    let mut list = plugins();
    list.clear();
    let Ok(names) = sys::opendir_names(rc::paths::PLUGINDIR.as_bytes()) else { return };
    for name in names {
        if name.first() == Some(&b'.') {
            continue;
        }
        let file = bcat!(rc::paths::PLUGINDIR, "/", name);
        let lib = match Library::open(&file) {
            Ok(lib) => lib,
            Err(e) => {
                eerror(Some(&bcat!("dlopen: ", e)));
                continue;
            }
        };
        if lib.has_hook() {
            list.push((name, lib));
        } else {
            eerror(Some(&bcat!(name, ": cannot find symbol `rc_plugin_hook'")));
            lib.close();
        }
    }
}

/// Apply what a plugin wrote to its pipe, as C does: `NAME=value` sets,
/// `NAME=` unsets.  The buffer is zeroed once and reused, so a short read
/// can run on into what an earlier one left.
fn read_environ(fd: i32) {
    const BUFSIZ: usize = 8192;
    let mut buffer = [0u8; BUFSIZ];
    // C's strlen/strsep may run past the buffer when it holds no NUL;
    // here they stop at its end.
    let strlen = |b: &[u8; BUFSIZ], at: usize| b[at..].iter().position(|&c| c == 0).unwrap_or(BUFSIZ - at);
    loop {
        let nr = sys::read_raw(fd, &mut buffer);
        if nr <= 0 {
            break;
        }
        let nr = nr as usize;
        let mut p = 0;
        while p < BUFSIZ && buffer[p] != 0 && p < nr {
            // strsep(&p, "="): the token runs to the first '=' or NUL.
            let len = strlen(&buffer, p);
            let token = buffer[p..p + len].to_vec();
            let Some(eq) = token.iter().position(|&c| c == b'=') else {
                // QUIRK: a record without '=' leaves strsep's pointer
                // NULL, which C then dereferences.
                // (Rust's runtime catches SIGSEGV, so drop its handler.)
                sys::default_signals(&[sys::libc::SIGSEGV]);
                let _ = sys::kill(sys::getpid(), sys::libc::SIGSEGV);
                stdio::exit(139);
            };
            // strsep ends the token at the '=' (which a later short read
            // may see again).
            buffer[p + eq] = 0;
            let name = &token[..eq];
            env::unset_bytes(name);
            p += eq + 1;
            if p < BUFSIZ && buffer[p] != 0 {
                let len = strlen(&buffer, p);
                env::set_bytes(name, &buffer[p..p + len]);
                p += len + 1;
            } else {
                p += 1;
            }
        }
    }
}

/// `rc_plugin_run(hook, value)`: call every plugin's hook, each in its own
/// child process, and take on the environment it sends back.
pub fn rc_plugin_run(hook: i32, value: &[u8]) {
    if rc_in_plugin() {
        return;
    }
    // Out of the mutex while the hooks run, so an exit on the way (which
    // unloads the plugins) can't deadlock.
    let list = std::mem::take(&mut *plugins());
    run_hooks(&list, hook, value);
    *plugins() = list;
}

fn run_hooks(list: &[(Vec<u8>, Library)], hook: i32, value: &[u8]) {
    use sys::libc;
    for (_, lib) in list {
        let (r, w) = match sys::pipe2_cloexec() {
            Ok(p) => p,
            Err(e) => {
                eerror(Some(&bcat!("pipe2: ", e.strerror())));
                return;
            }
        };
        let r = std::os::fd::IntoRawFd::into_raw_fd(r);
        let w = std::os::fd::IntoRawFd::into_raw_fd(w);
        let old = sys::block_all_signals();
        let pid = match sys::fork() {
            Ok(pid) => pid,
            Err(e) => {
                eerror(Some(&bcat!("fork: ", e.strerror())));
                // (C leaves the signals blocked and the pipe open here.)
                break;
            }
        };
        if pid == 0 {
            sys::default_signals(&[
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGUSR1,
                libc::SIGWINCH,
            ]);
            sys::restore_sigmask(&old);
            IN_PLUGIN.store(true, SeqCst);
            sys::close_raw(r);
            let retval = lib.call_hook(hook, value, w);
            stdio::exit(retval);
        }
        sys::restore_sigmask(&old);
        sys::close_raw(w);
        read_environ(r);
        sys::close_raw(r);
        exec::rc_waitpid(pid);
    }
}

/// `rc_plugin_unload()`.
pub fn rc_plugin_unload() {
    for (_, lib) in plugins().drain(..) {
        lib.close();
    }
}

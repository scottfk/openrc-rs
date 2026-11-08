//! `dlopen()` and friends, for OpenRC's plugins (`shared/plugin.c`): a
//! plugin is a shared object exporting `int rc_plugin_hook(RC_HOOK, const
//! char *)` that may write `NAME=value\0` records to librc's
//! `FILE *rc_environ_fd`.

use std::ffi::{CStr, c_char, c_int, c_void};

use crate::bytes::to_cstring;
use crate::sys::libc;

/// The type of `rc_plugin_hook`.
type HookFn = unsafe extern "C" fn(c_int, *const c_char) -> c_int;

/// A `dlopen()`ed object.
#[derive(Debug)]
pub struct Library {
    handle: *mut c_void,
}

// SAFETY: a dlopen handle is just a token; dlsym/dlclose are thread-safe.
unsafe impl Send for Library {}

fn dlerror() -> Vec<u8> {
    // SAFETY: dlerror returns null or a NUL-terminated string.
    let e = unsafe { libc::dlerror() };
    if e.is_null() {
        return Vec::new();
    }
    // SAFETY: non-null, NUL-terminated (see above).
    unsafe { CStr::from_ptr(e) }.to_bytes().to_vec()
}

impl Library {
    /// `dlopen(path, RTLD_LAZY)`; Err(dlerror()) on failure.
    pub fn open(path: &[u8]) -> Result<Library, Vec<u8>> {
        let p = to_cstring(path);
        // SAFETY: valid C string.  Loading runs the object's constructors,
        // which is what loading a plugin means.
        let handle = unsafe { libc::dlopen(p.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() { Err(dlerror()) } else { Ok(Library { handle }) }
    }

    fn sym(&self, name: &CStr) -> *mut c_void {
        // SAFETY: handle is a live dlopen handle; name is a C string.
        unsafe { libc::dlsym(self.handle, name.as_ptr()) }
    }

    /// Does the object (or a library it loaded) define `rc_plugin_hook`?
    pub fn has_hook(&self) -> bool {
        !self.sym(c"rc_plugin_hook").is_null()
    }

    /// Call `rc_plugin_hook(hook, value)` with `rc_environ_fd` (as the
    /// plugin sees it) open on `environ_fd`, and close it afterwards.  For
    /// the plugin's child process: the plugin's code runs here.
    ///
    /// Deviation: C sets openrc-run's own `rc_environ_fd`, which a plugin
    /// binds to when it is loaded.  openrc-rs doesn't link librc, so the
    /// variable is looked up through the plugin: the plugin has to link
    /// librc.so.1 (`-lrc`) to see it.
    pub fn call_hook(&self, hook: i32, value: Option<&[u8]>, environ_fd: i32) -> i32 {
        let f = self.sym(c"rc_plugin_hook");
        assert!(!f.is_null(), "plugins are only kept when they have a hook");
        let var = self.sym(c"rc_environ_fd").cast::<*mut libc::FILE>();
        let v = value.map(to_cstring);
        // SAFETY: f is `rc_plugin_hook`, whose type the plugin ABI fixes;
        // var (if found) is librc's `FILE *rc_environ_fd`; fdopen takes
        // ownership of environ_fd and fclose releases it.
        unsafe {
            let fp = libc::fdopen(environ_fd, c"w".as_ptr());
            if !var.is_null() {
                *var = fp;
            }
            let hook_fn: HookFn = std::mem::transmute::<*mut c_void, HookFn>(f);
            let retval = hook_fn(hook, v.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()));
            let fp = if var.is_null() { fp } else { std::mem::replace(&mut *var, std::ptr::null_mut()) };
            if !fp.is_null() {
                libc::fclose(fp);
            }
            retval
        }
    }

    /// `dlclose()`.
    pub fn close(self) {
        // SAFETY: handle is live and closed exactly once (self is consumed).
        unsafe { libc::dlclose(self.handle) };
    }
}

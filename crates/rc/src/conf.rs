//! `rc.conf` parsing (`librc-misc.c`).
//!
//! OpenRC does not run a shell to read `rc.conf`; it uses a small
//! line-based parser.  This is a mechanical port of it, keeping the same
//! list operations so that precedence between the various config files
//! behaves the same, quirks included.

use std::path::Path;
use std::sync::Mutex;

use openrc_compat::bytes::{as_path, c_str_prefix};

use crate::dirs::rc_is_user;
use crate::misc::rc_proc_getent;
use crate::paths::rc_conf_old;

/// An `RC_STRINGLIST` of `key=value` entries, in order.
pub type ConfigList = Vec<Vec<u8>>;

/// Split file contents as `getline()` would: lines without their `\n`,
/// each cut at its first NUL (C string semantics).
fn split_lines(data: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = data.split(|&b| b == b'\n').map(|l| c_str_prefix(l).to_vec()).collect();
    if data.ends_with(b"\n") || data.is_empty() {
        lines.pop();
    }
    lines
}

/// The parsing half of `config_list()`: keep assignment lines, with
/// leading blanks and `#` comments stripped (values are not unquoted).
fn parse_list(data: &[u8]) -> ConfigList {
    let mut list = Vec::new();
    for line in split_lines(data) {
        let start = line.iter().position(|&b| b != b' ' && b != b'\t').unwrap_or(line.len());
        let p = &line[start..];
        // QUIRK: everything after the first '#' is dropped, even inside quotes.
        let token = match p.iter().position(|&b| b == b'#') {
            Some(i) => &p[..i],
            None => p,
        };
        if token.len() > 1 && token.contains(&b'=') {
            let mut t = token.to_vec();
            if t.last() == Some(&b'\n') {
                t.pop();
            }
            list.push(t);
        }
    }
    list
}

/// `config_list(dirfd, path)`: the assignment lines of a file, opened
/// relative to `dir` (`None` = AT_FDCWD).  Read errors end the input, as
/// when `getline` fails on a directory; a missing file gives no lines.
pub fn config_list_at(dir: Option<std::os::fd::BorrowedFd<'_>>, path: &[u8]) -> ConfigList {
    use std::io::Read;

    use openrc_compat::sys::{self, libc};
    let Ok(fd) = sys::openat(dir, path, libc::O_RDONLY, 0o666) else { return Vec::new() };
    let mut f = std::fs::File::from(fd);
    let mut data = Vec::new();
    let _ = f.read_to_end(&mut data);
    parse_list(&data)
}

/// [`config_list_at`] on a raw dirfd as C passes it (-1 fails with EBADF,
/// giving an empty list).
pub fn config_list_raw(dirfd: i32, path: &[u8]) -> ConfigList {
    use std::io::Read;

    use openrc_compat::sys::{self, libc};
    let Ok(fd) = sys::at::openat(dirfd, path, libc::O_RDONLY, 0o666) else { return Vec::new() };
    let mut f = std::fs::File::from(fd);
    let mut data = Vec::new();
    let _ = f.read_to_end(&mut data);
    parse_list(&data)
}

/// `rc_config_list()`: [`config_list_at`] relative to the cwd.
pub fn rc_config_list(path: &Path) -> ConfigList {
    use std::os::unix::ffi::OsStrExt;
    config_list_at(None, path.as_os_str().as_bytes())
}

/// Index of the first entry for `key` (`key=...`).
fn find_key(list: &ConfigList, key: &[u8]) -> Option<usize> {
    list.iter().position(|l| l.starts_with(key) && l.get(key.len()) == Some(&b'='))
}

/// `rc_config_set_value()`: normalise one raw line and add it, replacing an
/// earlier value for the same key.
fn set_value(config: &mut ConfigList, value: &[u8]) {
    let off = if value.starts_with(b"export ") { 7 } else { 0 };
    let p = &value[off..];
    let eq = p.iter().position(|&b| b == b'=').unwrap_or(p.len());
    let entry = &p[..eq];
    let rest = p.get(eq + 1..).unwrap_or(&[]);

    let token: Option<&[u8]> = if rest.first() == Some(&b'$') {
        // QUIRK: meant to "preserve shell colouring", but strsep() has
        // already cut `value` at the '=', so this takes the text *before*
        // the '=' (the key, plus any "export " prefix) as the value.
        Some(&value[..off + eq])
    } else {
        // Bash variables are usually quoted: take the first non-empty
        // piece between quote characters.
        let mut cur = Some(rest);
        let mut tok = None;
        while let Some(s) = cur {
            let (t, next) = match s.iter().position(|&b| b == b'"' || b == b'\'') {
                Some(i) => (&s[..i], Some(&s[i + 1..])),
                None => (s, None),
            };
            cur = next;
            if !t.is_empty() {
                tok = Some(t);
                break;
            }
        }
        tok
    };

    let mut newline = entry.to_vec();
    newline.push(b'=');
    if let Some(t) = token {
        let t = t.strip_suffix(b"\n").unwrap_or(t);
        newline.extend_from_slice(t);
    }

    // In shells the last assignment wins, so replace any earlier value.
    match find_key(config, entry) {
        Some(i) => config[i] = newline,
        None => config.push(newline),
    }
}

/// `config_load()` / `rc_config_load()`: parse a file into `key=value`.
pub fn rc_config_load(path: &Path) -> ConfigList {
    let mut config = Vec::new();
    for line in rc_config_list(path) {
        set_value(&mut config, &line);
    }
    config
}

/// `rc_config_value()`: the value of the first entry for `key`.
pub fn rc_config_value<'a>(list: &'a ConfigList, key: &str) -> Option<&'a [u8]> {
    find_key(list, key.as_bytes()).map(|i| &list[i][key.len() + 1..])
}

/// `rc_config_directory()`: merge `<dir>/rc.conf.d/*.conf`, in sorted order.
fn config_directory(config: &mut ConfigList, dir: std::os::fd::BorrowedFd<'_>) {
    use std::os::fd::AsFd;

    use openrc_compat::sys::{self, libc};
    // do_opendirat(): openat(O_RDONLY | O_DIRECTORY).
    let Ok(dfd) = sys::openat(Some(dir), b"rc.conf.d", libc::O_RDONLY | libc::O_DIRECTORY, 0) else { return };
    let Ok(dup) = dfd.try_clone() else { return };
    let Ok(names) = sys::readdir_names(dup) else { return };
    let mut files: Vec<Vec<u8>> = Vec::new();
    for name in names {
        // fnmatch("*.conf", name, FNM_PATHNAME); dotfiles match too.
        if name.ends_with(b".conf") && !files.contains(&name) {
            files.push(name);
        }
    }
    files.sort();
    for name in files {
        for line in config_list_at(Some(dfd.as_fd()), &name) {
            set_value(config, &line);
        }
    }
}

/// `rc_config_kcl()`: options that can be overridden on the kernel
/// command line.
fn config_kcl(config: &mut ConfigList) {
    for name in ["rc_interactive", "rc_parallel"] {
        let Some(value) = rc_proc_getent(name) else { continue };
        let mut tmp = name.as_bytes().to_vec();
        tmp.push(b'=');
        tmp.extend_from_slice(&value);
        if let Some(i) = find_key(config, name.as_bytes()) {
            // rc_stringlist_delete(): removes the first *equal* entry.
            let old = config[i].clone();
            if let Some(j) = config.iter().position(|l| *l == old) {
                config.remove(j);
            }
        }
        config.push(tmp);
    }
}

/// Cache of the merged configuration (the C `static RC_STRINGLIST *rc_conf`).
static RC_CONF: Mutex<Option<ConfigList>> = Mutex::new(None);

fn load_rc_conf() -> ConfigList {
    let mut conf: ConfigList = Vec::new();
    // rc_scriptdirfds() silently skips directories that cannot be opened.
    let dirs = crate::dirs::rc_scriptdirfds();
    // Upstream says "load configs in reverse order, so earlier entries
    // override later ones", but rc.conf files are concatenated and lookup
    // takes the first match.  QUIRK: so for keys set in more than one
    // rc.conf, the *lowest*-priority directory wins (rc.conf.d entries do
    // replace in place).
    for &fd in dirs.iter().rev() {
        // SAFETY: the script dir fds stay open for the life of the process.
        let dir = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
        let mut loaded = Vec::new();
        for line in config_list_at(Some(dir), b"rc.conf") {
            set_value(&mut loaded, &line);
        }
        conf.extend(loaded);
        config_directory(&mut conf, dir);
    }

    // Support old configs.
    let old = rc_conf_old();
    if !rc_is_user() && as_path(&old).exists() {
        conf.extend(rc_config_load(as_path(&old)));
    }

    config_kcl(&mut conf);

    // Convert old uppercase keys to lowercase.
    for line in &mut conf {
        for b in line.iter_mut() {
            if *b == b'=' {
                break;
            }
            b.make_ascii_lowercase();
        }
        // The C ABI hands out pointers into these strings (rc_conf_value),
        // so keep a NUL just past the end.  The cache is never resized
        // after loading, so the buffers never move.
        line.reserve_exact(1);
        line.spare_capacity_mut()[0].write(0);
    }
    conf
}

/// Run `f` on the cached merged configuration, loading it on first use.
pub fn with_rc_conf<R>(f: impl FnOnce(&mut ConfigList) -> R) -> R {
    let mut guard = RC_CONF.lock().unwrap_or_else(|e| e.into_inner());
    let conf = guard.get_or_insert_with(load_rc_conf);
    f(conf)
}

/// `rc_conf_value()` for the C ABI: a pointer to the NUL-terminated value
/// inside the cache, valid for the life of the process (the C function
/// returns a pointer into its cache too, which callers must not free).
pub fn rc_conf_value_ptr(setting: &str) -> Option<*const u8> {
    with_rc_conf(|c| {
        let i = find_key(c, setting.as_bytes())?;
        // SAFETY: in bounds (the key and '=' precede the value).
        Some(unsafe { c[i].as_ptr().add(setting.len() + 1) })
    })
}

/// `rc_conf_value()`: a setting from `rc.conf` (and `rc.conf.d`, the old
/// `conf.d/rc`, and the kernel command line).
pub fn rc_conf_value(setting: &str) -> Option<Vec<u8>> {
    with_rc_conf(|c| rc_config_value(c, setting).map(<[u8]>::to_vec))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(lines: &[&str]) -> ConfigList {
        let mut c = Vec::new();
        for l in lines {
            set_value(&mut c, l.as_bytes());
        }
        c
    }

    #[test]
    fn quoting() {
        let c = set(&["a=\"x y\"", "b='z'", "c=plain", "d=", "export e=\"1\""]);
        assert_eq!(c, vec![b"a=x y".to_vec(), b"b=z".to_vec(), b"c=plain".to_vec(), b"d=".to_vec(), b"e=1".to_vec()]);
    }

    #[test]
    fn last_assignment_wins_in_place() {
        let c = set(&["a=1", "b=2", "a=3"]);
        assert_eq!(c, vec![b"a=3".to_vec(), b"b=2".to_vec()]);
    }

    #[test]
    fn dollar_quirk() {
        assert_eq!(set(&["a=$b"]), vec![b"a=a".to_vec()]);
        assert_eq!(set(&["export a=$b"]), vec![b"a=export a".to_vec()]);
    }

    #[test]
    fn list_strips_comments_and_blanks() {
        let dir = std::env::temp_dir().join(format!("rc-conf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rc.conf");
        std::fs::write(&f, "# c\n  \trc_sys=\"lxc\" # trailing\nnoeq\nx=1#y\n=\n").unwrap();
        assert_eq!(rc_config_list(&f), vec![b"rc_sys=\"lxc\" ".to_vec(), b"x=1".to_vec()]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

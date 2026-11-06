//! `seedrng`: port of `src/seedrng/seedrng.c`.  Seeds the kernel RNG from
//! seed files saved at the last shutdown, then saves a new seed (mixed
//! with BLAKE2s) for the next boot.

use std::os::fd::{AsFd, BorrowedFd};

use openrc_compat::bcat;
use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::num::strtoul;
use openrc_compat::sys::{self, Errno, libc};
use openrc_einfo::{eerror, eerrorx, einfo, ewarn};

use crate::shared::{Usage, getoptstring_common};

const DEFAULT_SEED_DIR: &str = "/var/lib/seedrng";
const CREDITABLE_SEED: &[u8] = b"seed.credit";
const NON_CREDITABLE_SEED: &[u8] = b"seed.no-credit";
const MAX_SEED_LEN: usize = 512;
const MIN_SEED_LEN: usize = blake2s::HASH_LEN;
const GRND_NONBLOCK: u32 = 0x0001;
const GRND_INSECURE: u32 = 0x0004;

const LONGOPT_SEED_DIR: i32 = 0x101;
const LONGOPT_SKIP_CREDIT: i32 = 0x102;

const GETOPTSTRING: &str = getoptstring_common!();
const LONGOPTS: &[LongOpt] = &[
    LongOpt { name: "seed-dir", has_arg: HasArg::Required, val: LONGOPT_SEED_DIR },
    LongOpt { name: "skip-credit", has_arg: HasArg::No, val: LONGOPT_SKIP_CREDIT },
];
const LONGOPTS_HELP: &[&str] =
    &["Directory for seed files (default: /var/lib/seedrng)", "Skip crediting entropy of seeds"];

/// BLAKE2s (RFC 7693), as implemented in seedrng.c.
mod blake2s {
    pub const BLOCK_LEN: usize = 64;
    pub const HASH_LEN: usize = 32;

    const IV: [u32; 8] =
        [0x6A09E667, 0xBB67AE85, 0x3C6EF372, 0xA54FF53A, 0x510E527F, 0x9B05688C, 0x1F83D9AB, 0x5BE0CD19];
    const SIGMA: [[usize; 16]; 10] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
    ];

    #[derive(Debug, Clone)]
    pub struct State {
        h: [u32; 8],
        t: [u32; 2],
        f: [u32; 2],
        buf: [u8; BLOCK_LEN],
        buflen: usize,
        outlen: usize,
    }

    impl State {
        pub fn new(outlen: usize) -> State {
            let mut h = IV;
            h[0] ^= 0x0101_0000 | outlen as u32;
            State { h, t: [0; 2], f: [0; 2], buf: [0; BLOCK_LEN], buflen: 0, outlen }
        }

        fn compress(&mut self, blocks: &[u8], inc: u32) {
            for block in blocks.as_chunks::<BLOCK_LEN>().0 {
                self.t[0] = self.t[0].wrapping_add(inc);
                self.t[1] = self.t[1].wrapping_add(u32::from(self.t[0] < inc));
                let mut m = [0u32; 16];
                for (i, w) in m.iter_mut().enumerate() {
                    *w = u32::from_le_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
                }
                let mut v = [0u32; 16];
                v[..8].copy_from_slice(&self.h);
                v[8..12].copy_from_slice(&IV[..4]);
                v[12] = IV[4] ^ self.t[0];
                v[13] = IV[5] ^ self.t[1];
                v[14] = IV[6] ^ self.f[0];
                v[15] = IV[7] ^ self.f[1];
                for s in &SIGMA {
                    let mut g = |i: usize, a: usize, b: usize, c: usize, d: usize| {
                        v[a] = v[a].wrapping_add(v[b]).wrapping_add(m[s[2 * i]]);
                        v[d] = (v[d] ^ v[a]).rotate_right(16);
                        v[c] = v[c].wrapping_add(v[d]);
                        v[b] = (v[b] ^ v[c]).rotate_right(12);
                        v[a] = v[a].wrapping_add(v[b]).wrapping_add(m[s[2 * i + 1]]);
                        v[d] = (v[d] ^ v[a]).rotate_right(8);
                        v[c] = v[c].wrapping_add(v[d]);
                        v[b] = (v[b] ^ v[c]).rotate_right(7);
                    };
                    g(0, 0, 4, 8, 12);
                    g(1, 1, 5, 9, 13);
                    g(2, 2, 6, 10, 14);
                    g(3, 3, 7, 11, 15);
                    g(4, 0, 5, 10, 15);
                    g(5, 1, 6, 11, 12);
                    g(6, 2, 7, 8, 13);
                    g(7, 3, 4, 9, 14);
                }
                for i in 0..8 {
                    self.h[i] ^= v[i] ^ v[i + 8];
                }
            }
        }

        pub fn update(&mut self, mut input: &[u8]) {
            if input.is_empty() {
                return;
            }
            let fill = BLOCK_LEN - self.buflen;
            if input.len() > fill {
                self.buf[self.buflen..].copy_from_slice(&input[..fill]);
                let buf = self.buf;
                self.compress(&buf, BLOCK_LEN as u32);
                self.buflen = 0;
                input = &input[fill..];
            }
            if input.len() > BLOCK_LEN {
                let nblocks = input.len().div_ceil(BLOCK_LEN);
                let n = BLOCK_LEN * (nblocks - 1);
                self.compress(&input[..n], BLOCK_LEN as u32);
                input = &input[n..];
            }
            self.buf[self.buflen..self.buflen + input.len()].copy_from_slice(input);
            self.buflen += input.len();
        }

        pub fn finalize(mut self) -> Vec<u8> {
            self.f[0] = u32::MAX;
            self.buf[self.buflen..].fill(0);
            let buf = self.buf;
            self.compress(&buf, self.buflen as u32);
            self.h.iter().flat_map(|w| w.to_le_bytes()).take(self.outlen).collect()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn hex(b: &[u8]) -> String {
            b.iter().map(|x| format!("{x:02x}")).collect()
        }

        #[test]
        fn rfc7693_vectors() {
            let h = |data: &[u8]| {
                let mut s = State::new(HASH_LEN);
                s.update(data);
                hex(&s.finalize())
            };
            assert_eq!(h(b""), "69217a3079908094e11121d042354a7c1f55b6482ca1a51e1b250dfd1ed0eef9");
            assert_eq!(h(b"abc"), "508c5e8c327c14e2e1a72ba34eeb452f37458b209ed63a294d999b4c86675982");
            // Multi-block input, also fed in pieces.
            let data: Vec<u8> = (0..=255u8).collect();
            let whole = h(&data);
            let mut s = State::new(HASH_LEN);
            for chunk in data.chunks(7) {
                s.update(chunk);
            }
            assert_eq!(hex(&s.finalize()), whole);
        }
    }
}

fn read_full(fd: BorrowedFd<'_>, buf: &mut [u8]) -> Result<usize, Errno> {
    let mut total = 0;
    while total < buf.len() {
        match sys::read_once(fd, &mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(e) if e.0 == libc::EINTR => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

fn write_full(fd: BorrowedFd<'_>, buf: &[u8]) -> Result<usize, Errno> {
    let mut total = 0;
    while total < buf.len() {
        match sys::write_once(fd, &buf[total..]) {
            Ok(n) => total += n,
            Err(e) if e.0 == libc::EINTR => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

fn getrandom_full(buf: &mut [u8], flags: u32) -> Result<usize, Errno> {
    let mut total = 0;
    loop {
        match sys::getrandom(&mut buf[total..], flags) {
            Ok(n) => total += n,
            Err(e) if e.0 == libc::EINTR => continue,
            Err(e) => return Err(e),
        }
        if total == buf.len() {
            return Ok(total);
        }
    }
}

fn determine_optimal_seed_len(applet: &[u8]) -> usize {
    let fd = sys::openat(None, b"/proc/sys/kernel/random/poolsize", libc::O_RDONLY, 0);
    let mut s = [0u8; 10];
    let read = match &fd {
        Ok(fd) => read_full(fd.as_fd(), &mut s),
        Err(e) => Err(*e),
    };
    let ret = match read {
        Err(e) => {
            ewarn(Some(&bcat!(
                applet,
                ": Unable to determine pool size, falling back to ",
                (MIN_SEED_LEN * 8).to_string(),
                " bits: ",
                e.strerror()
            )));
            MIN_SEED_LEN
        }
        Ok(n) => {
            let text = openrc_compat::bytes::c_str_prefix(&s[..n]);
            strtoul(text, 10).value.div_ceil(8) as usize
        }
    };
    ret.clamp(MIN_SEED_LEN, MAX_SEED_LEN)
}

/// `read_new_seed()`: Ok(creditable), or the errno.
fn read_new_seed(seed: &mut [u8]) -> Result<bool, Errno> {
    let mut creditable = false;
    match getrandom_full(seed, GRND_NONBLOCK) {
        Ok(n) if n == seed.len() => return Ok(true),
        Err(e) if e.0 == libc::ENOSYS => {
            let fd = sys::openat(None, b"/dev/random", libc::O_RDONLY, 0)?;
            creditable = sys::poll_now(fd.as_fd(), libc::POLLIN) == 1;
        }
        _ => {
            if matches!(getrandom_full(seed, GRND_INSECURE), Ok(n) if n == seed.len()) {
                return Ok(false);
            }
        }
    }
    let fd = sys::openat(None, b"/dev/urandom", libc::O_RDONLY, 0)?;
    match read_full(fd.as_fd(), seed) {
        Ok(n) if n == seed.len() => Ok(creditable),
        // C: errno, or EIO if errno happens to be 0.
        Ok(_) => Err(Errno(libc::EIO)),
        Err(e) => Err(e),
    }
}

fn seed_rng(seed: &[u8], credit: bool) -> Result<(), Errno> {
    let fd = sys::openat(None, b"/dev/urandom", libc::O_RDONLY, 0)?;
    let bits = if credit { (seed.len() * 8) as i32 } else { 0 };
    sys::rnd_add_entropy(fd.as_fd(), bits, seed)
}

/// `seed_from_file_if_exists()`: false on failure.
fn seed_from_file(applet: &[u8], name: &[u8], dfd: BorrowedFd<'_>, credit: bool, hash: &mut blake2s::State) -> bool {
    let fd = match sys::openat(Some(dfd), name, libc::O_RDONLY, 0) {
        Ok(fd) => fd,
        Err(e) if e.0 == libc::ENOENT => return true,
        Err(e) => {
            eerror(Some(&bcat!(applet, ": Unable to open seed file: ", e.strerror())));
            return false;
        }
    };
    let mut seed = [0u8; MAX_SEED_LEN];
    let len = match read_full(fd.as_fd(), &mut seed) {
        Ok(n) => n,
        Err(e) => {
            eerror(Some(&bcat!(applet, ": Unable to read seed file: ", e.strerror())));
            return false;
        }
    };
    let removed = sys::unlinkat(Some(dfd), name, 0).and_then(|()| sys::fsync(dfd));
    if let Err(e) = removed
        && len > 0
    {
        eerror(Some(&bcat!(applet, ": Unable to remove seed after reading, so not seeding: ", e.strerror())));
        return false;
    }
    if len == 0 {
        return true;
    }
    hash.update(&(len as isize).to_ne_bytes());
    hash.update(&seed[..len]);
    einfo(Some(format!("Seeding {} bits {} crediting", len * 8, if credit { "and" } else { "without" }).as_bytes()));
    if let Err(e) = seed_rng(&seed[..len], credit) {
        eerror(Some(&bcat!(applet, ": Unable to seed: ", e.strerror())));
        return false;
    }
    true
}

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
    const PREFIX: &[u8] = b"SeedRNG v1 Old+New Prefix";
    const FAILURE: &[u8] = b"SeedRNG v1 No New Seed Failure";

    let argc = args.len();
    let usage = Usage {
        applet: applet.to_vec(),
        getoptstring: GETOPTSTRING,
        longopts: LONGOPTS,
        longopts_help: LONGOPTS_HELP,
        extraopts: None,
        usagestring: None,
    };
    let longopts = usage.all_longopts();
    let mut seed_dir: Option<Vec<u8>> = None;
    let mut skip_credit = false;
    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match opt {
            LONGOPT_SEED_DIR => {
                if seed_dir.is_none() {
                    seed_dir = g.optarg.clone();
                }
            }
            LONGOPT_SKIP_CREDIT => skip_credit = true,
            _ => usage.common_option(opt, argc),
        }
    }
    let seed_dir = seed_dir.unwrap_or_else(|| DEFAULT_SEED_DIR.as_bytes().to_vec());
    if sys::getuid() != 0 {
        eerrorx(Some(&bcat!(applet, ": superuser access is required")));
    }
    sys::umask(0o077);

    let mut hash = blake2s::State::new(blake2s::HASH_LEN);
    hash.update(PREFIX);
    hash.update(&sys::timespec_bytes(libc::CLOCK_REALTIME));
    hash.update(&sys::timespec_bytes(libc::CLOCK_BOOTTIME));

    if let Err(e) = sys::mkdir(&seed_dir, 0o700)
        && e.0 != libc::EEXIST
    {
        eerrorx(Some(&bcat!(applet, ": Unable to create seed directory: ", e.strerror())));
    }
    let dfd = sys::openat(None, &seed_dir, libc::O_DIRECTORY | libc::O_RDONLY, 0)
        .and_then(|fd| sys::flock(fd.as_fd(), libc::LOCK_EX).map(|()| fd));
    let dfd = match dfd {
        Ok(fd) => fd,
        Err(e) => eerrorx(Some(&bcat!(applet, ": Unable to lock seed directory: ", e.strerror()))),
    };

    let mut program_ret = 0;
    if !seed_from_file(applet, NON_CREDITABLE_SEED, dfd.as_fd(), false, &mut hash) {
        program_ret |= 1 << 1;
    }
    if !seed_from_file(applet, CREDITABLE_SEED, dfd.as_fd(), !skip_credit, &mut hash) {
        program_ret |= 1 << 2;
    }

    let mut new_seed_len = determine_optimal_seed_len(applet);
    let mut new_seed = vec![0u8; MAX_SEED_LEN];
    let creditable = match read_new_seed(&mut new_seed[..new_seed_len]) {
        Ok(c) => c,
        Err(e) => {
            eerror(Some(&bcat!(applet, ": Unable to read new seed: ", e.strerror())));
            new_seed_len = blake2s::HASH_LEN;
            // strncpy: the message, NUL-padded to 32 bytes.
            new_seed[..new_seed_len].fill(0);
            new_seed[..FAILURE.len()].copy_from_slice(FAILURE);
            program_ret |= 1 << 3;
            false
        }
    };
    hash.update(&new_seed_len.to_ne_bytes());
    hash.update(&new_seed[..new_seed_len]);
    let digest = hash.finalize();
    new_seed[new_seed_len - blake2s::HASH_LEN..new_seed_len].copy_from_slice(&digest);

    einfo(Some(
        format!(
            "Saving {} bits of {} seed for next boot",
            new_seed_len * 8,
            if creditable { "creditable" } else { "non-creditable" }
        )
        .as_bytes(),
    ));
    let fd = match sys::openat(
        Some(dfd.as_fd()),
        NON_CREDITABLE_SEED,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o400,
    ) {
        Ok(fd) => fd,
        Err(e) => {
            eerror(Some(&bcat!(applet, ": Unable to open seed file for writing: ", e.strerror())));
            return program_ret | (1 << 4);
        }
    };
    let written = write_full(fd.as_fd(), &new_seed[..new_seed_len]).and_then(|n| {
        if n != new_seed_len {
            return Err(Errno::last());
        }
        sys::fsync(fd.as_fd())
    });
    if let Err(e) = written {
        eerror(Some(&bcat!(applet, ": Unable to write seed file: ", e.strerror())));
        return program_ret | (1 << 5);
    }
    if creditable && let Err(e) = sys::renameat(dfd.as_fd(), NON_CREDITABLE_SEED, CREDITABLE_SEED) {
        ewarn(Some(&bcat!(applet, ": Unable to make new seed creditable: ", e.strerror())));
        return program_ret | (1 << 6);
    }
    program_ret
}

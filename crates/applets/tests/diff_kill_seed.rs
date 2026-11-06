//! kill_all and seedrng vs the C reference.
//!
//! kill_all signals every process it can reach (and SIGSTOPs them all
//! even with --dry-run), so it only ever runs as PID 1 of a private PID
//! namespace whose only other processes are the sleepers started here.
//!
//! seedrng needs uid 0; in a user namespace it gets that, but crediting
//! entropy (RNDADDENTROPY) still fails with EPERM, which is compared too.

mod common;
use std::fs;
use std::path::Path;

use common::{Case, Ns};

/// PID 2 (looks like kthreadd: skipped), PID 3 in another session
/// (listed), PID 4 in our session (skipped), PID 5 another session.
const SLEEPERS: &str = "sleep 300 &\nsetsid sleep 300 &\nsleep 300 &\nsetsid sleep 300 &\nsleep 0.3";

fn ka(args: &[&str]) -> Case {
    Case::new("kill_all", args).pidns(SLEEPERS)
}

#[test]
fn kill_all() {
    if !Path::new("/usr/bin/unshare").exists() {
        eprintln!("SKIPPED: no /usr/bin/unshare");
        return;
    }
    let cases = vec![
        ka(&["-h"]),
        ka(&["-d"]),
        ka(&["--dry-run", "15"]),
        ka(&["-d", "-o", "3"]),
        ka(&["-d", "-o", "003"]),
        ka(&["-d", "-o", "3,5"]),
        ka(&["-d", "-o", "3;4:5"]),
        ka(&["-d", "--omit=5", "-o", "3"]),
        ka(&["-d", "-o", "x"]),
        ka(&["-d", "-o", "3,"]),
        ka(&["-d", "0"]),
        ka(&["-d", "32"]),
        ka(&["-d", "abc"]),
        ka(&["-d", "9x"]),
        ka(&["-d", "-q"]),
        ka(&[]),
        ka(&["15"]),
    ];
    common::check_all("kill_all", &cases);
}

fn seeds(r: &Path) {
    fs::create_dir(r.join("seeds")).unwrap();
    fs::write(r.join("seeds/seed.credit"), [7u8; 64]).unwrap();
    fs::write(r.join("seeds/seed.no-credit"), [9u8; 16]).unwrap();
    fs::create_dir(r.join("empty-seeds")).unwrap();
    fs::write(r.join("empty-seeds/seed.credit"), b"").unwrap();
    fs::write(r.join("notadir"), b"x").unwrap();
}

#[test]
fn seedrng() {
    let s = seeds as fn(&Path);
    let root = || Ns::default();
    let mut cases = vec![
        Case::new("seedrng", &["-h"]),
        Case::new("seedrng", &["--bogus"]),
        Case::new("seedrng", &["--seed-dir=/nonexistent"]),
        Case::new("seedrng", &["--seed-dir", "$ROOT/new"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/seeds"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/seeds", "--skip-credit"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/empty-seeds"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/seeds", "--seed-dir", "$ROOT/other"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/notadir"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed-dir", "$ROOT/no/parent"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["--seed", "$ROOT/new"]).sandbox(s).ns(root()),
        Case::new("seedrng", &["-q", "--seed-dir", "$ROOT/new"]).sandbox(s).ns(root()),
    ];
    // Uid 0 for real (the VM): crediting works there.
    if common::is_root() {
        cases.push(Case::new("seedrng", &["--seed-dir", "$ROOT/seeds"]).sandbox(s));
        cases.push(Case::new("seedrng", &["--seed-dir", "$ROOT/seeds", "--skip-credit"]).sandbox(s));
    } else {
        cases.push(Case::new("seedrng", &["--seed-dir", "$ROOT/seeds"]).sandbox(s));
    }
    common::check_all("seedrng", &cases);
}

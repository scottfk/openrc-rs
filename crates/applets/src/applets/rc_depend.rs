//! `rc-depend`: port of `src/rc-depend/rc-depend.c`.  Print the
//! dependencies of services, from the cached dependency tree.

use openrc_compat::getopt::{Getopt, HasArg, LongOpt};
use openrc_compat::{env, stdio};
use openrc_einfo::eerrorx;
use rc::deptree::{self, options};

use crate::shared::{Usage, default_runlevel, getoptstring_common, rc_deptree_load_updating};

const GETOPTSTRING: &str = concat!("aot:suTF:", getoptstring_common!());
const LONGOPTS: &[LongOpt] = &[
    LongOpt::new("starting", HasArg::No, b'a'),
    LongOpt::new("stopping", HasArg::No, b'o'),
    LongOpt::new("type", HasArg::Required, b't'),
    LongOpt::new("notrace", HasArg::No, b'T'),
    LongOpt::new("strict", HasArg::No, b's'),
    LongOpt::new("update", HasArg::No, b'u'),
    LongOpt::new("deptree-file", HasArg::Required, b'F'),
];
const LONGOPTS_HELP: &[&str] = &[
    "Order services as if runlevel is starting",
    "Order services as if runlevel is stopping",
    "Type(s) of dependency to list",
    "Don't trace service dependencies",
    "Only use what is in the runlevels",
    "Force an update of the dependency tree",
    "File to load cached deptree from",
];

pub fn main(applet: &[u8], args: Vec<Vec<u8>>) -> i32 {
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

    let mut opts = options::TRACE;
    let mut update = 0;
    let mut runlevel = env::get("RC_RUNLEVEL");
    let mut types: Vec<Vec<u8>> = Vec::new();
    let mut deptree_file: Option<Vec<u8>> = None;

    let mut g = Getopt::new(args, GETOPTSTRING);
    while let Some(opt) = g.next_long(&longopts) {
        match u8::try_from(opt).unwrap_or(0) {
            b'a' => opts |= options::START,
            b'o' => opts |= options::STOP,
            b's' => opts |= options::STRICT,
            // strsep(&optarg, ","): every piece, empty ones included.
            b't' => types.extend(g.optarg.as_deref().unwrap_or_default().split(|&b| b == b',').map(<[u8]>::to_vec)),
            b'u' => update = 1,
            // QUIRK: `&= RC_DEP_TRACE` (not `&= ~`): keeps tracing on and
            // drops any -a, -o or -s given before it.
            b'T' => opts &= options::TRACE,
            b'F' => deptree_file = g.optarg.clone(),
            _ => usage.common_option(opt, argc),
        }
    }

    let tree = match &deptree_file {
        Some(f) => deptree::rc_deptree_load_file(f),
        None => rc_deptree_load_updating(update, None),
    };
    let Some(tree) = tree else { eerrorx(Some(b"failed to load deptree")) };

    let runlevel = runlevel.take().unwrap_or_else(default_runlevel);

    // QUIRK: C checks each service with rc_deptree_depends(types = NULL)
    // and reports "no dependency info for service `%s'" when that returns
    // NULL with ENOENT, but it always returns a list, so every argument is
    // kept, known or not.
    let services: Vec<Vec<u8>> = g.args[g.optind..].to_vec();
    if services.is_empty() {
        if update != 0 {
            return 0;
        }
        eerrorx(Some(b"no services specified"));
    }

    // If we don't have any types, then supply some defaults.
    if types.is_empty() {
        types = vec![b"ineed".to_vec(), b"iuse".to_vec()];
    }

    let (depends, _) = deptree::rc_deptree_depends(&tree, Some(&types), &services, Some(&runlevel), opts);
    if !depends.is_empty() {
        stdio::out(&depends.join(&b' '));
        stdio::out(b"\n");
    }
    0
}

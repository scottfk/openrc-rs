//! String lists and `rc_config_value()` (Phase 2 step 5): librc.so.1 vs
//! the C reference, through rc-probe (tests/capi/rc-probe.c).
//!
//! rc-probe walks the TAILQ links itself after every change (`links=ok`),
//! so these check the list layout callers rely on as well as the values.

mod common;
use common::Case;

/// Split a "a b -- c d" script into rc-probe's argument list.
fn case(p: &std::path::Path, script: &str) -> Case {
    let args: Vec<&str> = script.split(' ').collect();
    let mut c = Case::programs("rc-probe", p.to_path_buf(), p.to_path_buf(), &args);
    c.ourlibs = true;
    c
}

#[test]
fn stringlist_functions() {
    let Some(p) = common::rc_probe() else { return };
    let scripts = [
        // Adding, duplicates, addu.
        "slnew -- sladd b -- sladd a -- sladd b -- sladdu b -- sladdu c -- sladd  -- sladdu  -- sladdu ",
        "slnew -- sladdu x -- sladdu x -- sladd x -- sladdu x",
        // Deleting: first match only; not found sets EEXIST.
        "slnew -- sladd a -- sladd b -- sladd a -- sldel a -- sldel a -- sldel a -- sldel nosuch -- sladd z",
        "slnew -- sladd only -- sldel only -- sladd again -- sldel again -- sldel again",
        "slnew -- sladd a -- sladd b -- sladd c -- sldel c -- sladd d -- sldel b -- sladd e",
        // Finding.
        "slnew -- sladd a -- sladd b -- slfind a -- slfind b -- slfind c -- slfind-null a -- slfind ",
        // A NULL value (xstrdup(NULL)).
        "slnew -- sladd a -- sladdnull -- sladd b -- slfree",
        // Splitting: empty pieces kept, any byte of SEP splits.
        "slsplit a,b,,c ,",
        "slsplit ,a, ,",
        "slsplit a:b,c;d :,;",
        "slsplit abc x",
        "slsplit abc ",
        "slsplit - ,",
        "slsplit ,,, ,",
        // Sorting: stable, C byte order, nodes moved (a held node stays
        // on the list), a new head each time.
        "slsplit d,b,a,c,b,A,_,1,,b ,: -- slhold c -- slsort -- slsort -- sladd 0 -- slsort -- sldel b -- slsort",
        // Stability: the held first "b" stays ahead of the other two.
        "slsplit b,c,b,a,b , -- slhold b -- slsort -- sldel b -- slsort",
        "slnew -- slsort -- sladd one -- slsort -- slhold one -- slsort",
        "slsplit z,y,x,w,v,u ,: -- slsort -- slhold x -- sldel x -- sladd x -- slsort",
        // Freeing (and rc_stringlist_free(NULL)).
        "slsplit a,b ,: -- slfree -- slnew -- slfree",
        // The rc_environ_fd variable.
        "environ",
    ];
    let cases: Vec<Case> = scripts.iter().map(|s| case(&p, s)).collect();
    common::check_all("stringlist", &cases);
}

#[test]
fn config_value() {
    let Some(p) = common::rc_probe() else { return };
    let scripts = [
        "slsplit a=1,b=2,a=3,ab=4,b,=5,c= , -- cfgval a -- cfgval b -- cfgval ab -- cfgval c -- cfgval d -- cfgval ",
        // The value starts after the line's first '=' (QUIRK).
        "slsplit a=b=c,a==x , -- cfgval a=b -- cfgval a -- cfgval a= -- cfgval =",
        "slsplit A=1,a=2 , -- cfgval a -- cfgval A",
        "slnew -- cfgval a",
    ];
    let cases: Vec<Case> = scripts.iter().map(|s| case(&p, s)).collect();
    common::check_all("config-value", &cases);
}

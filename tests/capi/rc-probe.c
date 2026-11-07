/*
 * rc-probe.c - call librc.so.1 functions from the command line, for
 * differential testing of openrc-rs's librc against the C reference.
 *
 * Usage: rc-probe CMD [ARGS...] [-- CMD [ARGS...]]...
 * Prints each result (and errno where the API sets it), so a run on the C
 * library and a run on ours can be compared byte for byte.
 *
 * Copyright (c) 2026 scottfk; BSD-2-Clause, see LICENSE.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <limits.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <rc.h>

static RC_DEPTREE *tree;
static RC_STRINGLIST *sl;	/* the list the sl* commands work on */
static RC_STRING *held;		/* a node kept across sl* commands */
static time_t age_to;

/* "-" is NULL; otherwise a comma-separated list (empty items kept). */
static RC_STRINGLIST *mklist(const char *s)
{
	RC_STRINGLIST *l;
	char *buf, *p, *tok;

	if (!strcmp(s, "-"))
		return NULL;
	l = rc_stringlist_new();
	if (!*s)
		return l;
	buf = p = strdup(s);
	while ((tok = strsep(&p, ",")))
		rc_stringlist_add(l, tok);
	free(buf);
	return l;
}

static void set_age(const char *path, time_t t)
{
	struct timespec ts[2] = { { t, 0 }, { t, 0 } };
	if (utimensat(AT_FDCWD, path, ts, AT_SYMLINK_NOFOLLOW) != 0)
		printf("age %s: %s\n", path, strerror(errno));
}

static int age_one(const char *path, const struct stat *st, int flag, struct FTW *f)
{
	(void)st; (void)flag; (void)f;
	set_age(path, age_to);
	return 0;
}

static const char *const deptypes[] = {
	"ineed", "needsme", "iuse", "usesme", "iwant", "wantsme", "iafter", "ibefore",
	"iprovide", "providedby", "keyword", "broken", "reexport", "config", NULL
};

static void list(const char *what, RC_STRINGLIST *l)
{
	RC_STRING *s;

	printf("%s:", what);
	if (!l) {
		printf(" (null)\n");
		return;
	}
	for (s = l->tqh_first; s; s = s->entries.tqe_next)
		printf(" [%s]", s->value);
	printf("\n");
	rc_stringlist_free(l);
}

static void str(const char *what, char *s)
{
	printf("%s: %s%s%s\n", what, s ? "[" : "", s ? s : "(null)", s ? "]" : "");
	free(s);
}

static void boolean(const char *what, bool b, int err)
{
	printf("%s: %s", what, b ? "true" : "false");
	if (!b)
		printf(" errno=%s", err ? strerror(err) : "0");
	printf("\n");
}

#define B(what, call) do { errno = 0; bool _b = (call); boolean(what, _b, errno); } while (0)

static const struct { const char *name; RC_SERVICE state; } states[] = {
	{ "stopped", RC_SERVICE_STOPPED }, { "started", RC_SERVICE_STARTED },
	{ "stopping", RC_SERVICE_STOPPING }, { "starting", RC_SERVICE_STARTING },
	{ "inactive", RC_SERVICE_INACTIVE }, { "hotplugged", RC_SERVICE_HOTPLUGGED },
	{ "failed", RC_SERVICE_FAILED }, { "scheduled", RC_SERVICE_SCHEDULED },
	{ "wasinactive", RC_SERVICE_WASINACTIVE }, { "crashed", RC_SERVICE_CRASHED },
	{ "bogus", (RC_SERVICE)0x4000 },
};

static RC_SERVICE state_of(const char *name)
{
	for (size_t i = 0; i < sizeof(states) / sizeof(states[0]); i++)
		if (strcmp(states[i].name, name) == 0)
			return states[i].state;
	return (RC_SERVICE)atoi(name);
}

/* "-" is NULL; otherwise a comma-separated list as a NULL-terminated argv. */
static const char **split_argv(const char *s)
{
	static const char *v[16];
	static char buf[1024];
	int n = 0;
	char *p, *tok;

	if (!strcmp(s, "-"))
		return NULL;
	snprintf(buf, sizeof(buf), "%s", s);
	p = buf;
	while ((tok = strsep(&p, ",")) && n < 15)
		v[n++] = tok;
	v[n] = NULL;
	return v;
}

static const char *nul(const char *s)
{
	return strcmp(s, "-") ? s : NULL;
}

/* Run one command at argv[0]; returns how many args it used. */
/* Print sl's values, and check every TAILQ link. */
static void sl_show(void)
{
	RC_STRING *s, **prev;
	bool ok = true;
	int i = 0, held_at = -1;

	if (!sl) {
		printf("sl: (null)\n");
		return;
	}
	printf("sl:");
	prev = &sl->tqh_first;
	for (s = sl->tqh_first; s; s = s->entries.tqe_next) {
		printf(" [%s]", s->value ? s->value : "(NULL)");
		if (s->entries.tqe_prev != prev)
			ok = false;
		if (s == held)
			held_at = i;
		prev = &s->entries.tqe_next;
		i++;
	}
	if (sl->tqh_last != prev)
		ok = false;
	printf(" links=%s", ok ? "ok" : "BAD");
	if (held)
		printf(held_at < 0 ? " held=[%s] (not on list)" : " held=[%s]@%d", held->value, held_at);
	printf("\n");
}

static int run(int argc, char **argv)
{
	const char *c = argv[0];
	const char *a = argc > 1 ? argv[1] : NULL;
	const char *b = argc > 2 ? argv[2] : NULL;

	if (!strcmp(c, "state")) {
		printf("state %s: 0x%x\n", a, rc_service_state(a));
		return 2;
	} else if (!strcmp(c, "mark")) {
		B("mark", rc_service_mark(a, state_of(b)));
		return 3;
	} else if (!strcmp(c, "in_state")) {
		list(a, rc_services_in_state(state_of(a)));
		return 2;
	} else if (!strcmp(c, "resolve")) {
		str("resolve", rc_service_resolve(a));
		return 2;
	} else if (!strcmp(c, "exists")) {
		B("exists", rc_service_exists(a));
		return 2;
	} else if (!strcmp(c, "describe")) {
		str("describe", rc_service_description(a, b && *b ? b : NULL));
		return 3;
	} else if (!strcmp(c, "extra")) {
		list("extra", rc_service_extra_commands(a));
		return 2;
	} else if (!strcmp(c, "in_runlevel")) {
		B("in_runlevel", rc_service_in_runlevel(a, b));
		return 3;
	} else if (!strcmp(c, "add")) {
		B("add", rc_service_add(a, b));
		return 3;
	} else if (!strcmp(c, "delete")) {
		B("delete", rc_service_delete(a, b));
		return 3;
	} else if (!strcmp(c, "schedule_start")) {
		B("schedule_start", rc_service_schedule_start(a, b));
		return 3;
	} else if (!strcmp(c, "schedule_clear")) {
		B("schedule_clear", rc_service_schedule_clear(a));
		return 2;
	} else if (!strcmp(c, "scheduled")) {
		list("scheduled", rc_services_scheduled(a));
		return 2;
	} else if (!strcmp(c, "scheduled_by")) {
		list("scheduled_by", rc_services_scheduled_by(a));
		return 2;
	} else if (!strcmp(c, "putenv")) {
		B("putenv", rc_service_putenv(a, b));
		return 3;
	} else if (!strcmp(c, "in_runlevel_list")) {
		list("services_in_runlevel", rc_services_in_runlevel(strcmp(a, "NULL") ? a : NULL));
		return 2;
	} else if (!strcmp(c, "stacked")) {
		list("stacked", rc_services_in_runlevel_stacked(strcmp(a, "NULL") ? a : NULL));
		return 2;
	} else if (!strcmp(c, "stacks")) {
		list("stacks", rc_runlevel_stacks(a));
		return 2;
	} else if (!strcmp(c, "stack")) {
		B("stack", rc_runlevel_stack(a, b));
		return 3;
	} else if (!strcmp(c, "unstack")) {
		B("unstack", rc_runlevel_unstack(a, b));
		return 3;
	} else if (!strcmp(c, "levels")) {
		list("runlevels", rc_runlevel_list());
		return 1;
	} else if (!strcmp(c, "level_exists")) {
		B("level_exists", rc_runlevel_exists(a));
		return 2;
	} else if (!strcmp(c, "get")) {
		str("runlevel", rc_runlevel_get());
		return 1;
	} else if (!strcmp(c, "set")) {
		B("set", rc_runlevel_set(a));
		return 2;
	} else if (!strcmp(c, "starting")) {
		printf("starting: %d stopping: %d\n", rc_runlevel_starting(), rc_runlevel_stopping());
		return 1;
	} else if (!strcmp(c, "daemon_set")) {
		/* daemon_set SVC EXEC ARGV PIDFILE STARTED */
		B("daemon_set", rc_service_daemon_set(a, nul(b), split_argv(argv[3]), nul(argv[4]), atoi(argv[5])));
		return 6;
	} else if (!strcmp(c, "started_daemon")) {
		/* started_daemon SVC EXEC ARGV INDX */
		B("started_daemon", rc_service_started_daemon(a, nul(b), split_argv(argv[3]), atoi(argv[4])));
		return 5;
	} else if (!strcmp(c, "crashed")) {
		B("crashed", rc_service_daemons_crashed(a));
		return 2;
	} else if (!strcmp(c, "find_pids")) {
		/* find_pids EXEC ARGV UID PID */
		RC_PIDLIST *pids = rc_find_pids(nul(a), split_argv(b), (uid_t)atoi(argv[3]), (pid_t)atoi(argv[4]));
		RC_PID *pi, *next;
		printf("find_pids:");
		if (!pids) {
			printf(" (null)\n");
			return 5;
		}
		for (pi = pids->lh_first; pi; pi = next) {
			next = pi->entries.le_next;
			printf(" %d", (int)pi->pid);
			free(pi);
		}
		free(pids);
		printf("\n");
		return 5;
	} else if (!strcmp(c, "update")) {
		B("update", rc_deptree_update());
		return 1;
	} else if (!strcmp(c, "update_needed")) {
		char file[PATH_MAX] = "";
		time_t newest = -1;
		bool needed = rc_deptree_update_needed(&newest, file);
		printf("update_needed: %s newest=%s file=[%s]\n", needed ? "true" : "false",
		       newest == -1 ? "unset" : "set", file);
		return 1;
	} else if (!strcmp(c, "load")) {
		rc_deptree_free(tree);
		tree = rc_deptree_load();
		printf("load: %s\n", tree ? "ok" : "(null)");
		return 1;
	} else if (!strcmp(c, "load_file")) {
		rc_deptree_free(tree);
		tree = rc_deptree_load_file(a);
		printf("load_file: %s\n", tree ? "ok" : "(null)");
		return 2;
	} else if (!strcmp(c, "depend")) {
		errno = 0;
		RC_STRINGLIST *l = rc_deptree_depend(tree, a, b);
		int e = errno;
		list("depend", l);
		if (e)
			printf("errno=%s\n", strerror(e));
		return 3;
	} else if (!strcmp(c, "dump")) {
		/* dump SVC,SVC...: every non-empty dependency list */
		RC_STRINGLIST *svcs = mklist(a);
		RC_STRING *sv;
		for (sv = svcs->tqh_first; sv; sv = sv->entries.tqe_next)
			for (const char *const *t = deptypes; *t; t++) {
				RC_STRINGLIST *l = rc_deptree_depend(tree, sv->value, *t);
				char what[256];
				if (!l->tqh_first) {
					rc_stringlist_free(l);
					continue;
				}
				snprintf(what, sizeof(what), "%s %s", sv->value, *t);
				list(what, l);
			}
		rc_stringlist_free(svcs);
		return 2;
	} else if (!strcmp(c, "depends")) {
		/* depends TYPES SERVICES RUNLEVEL OPTIONS */
		RC_STRINGLIST *types = mklist(a), *svcs = mklist(b);
		char what[512];
		errno = 0;
		RC_STRINGLIST *l = rc_deptree_depends(tree, types, svcs, nul(argv[3]), atoi(argv[4]));
		int e = errno;
		snprintf(what, sizeof(what), "depends %s %s %s %s", a, b, argv[3], argv[4]);
		list(what, l);
		if (e == ENOENT)
			printf("errno=%s\n", strerror(e));
		rc_stringlist_free(types);
		rc_stringlist_free(svcs);
		return 5;
	} else if (!strcmp(c, "order")) {
		char what[256];
		snprintf(what, sizeof(what), "order %s %s", a, b);
		list(what, rc_deptree_order(tree, a, atoi(b)));
		return 3;
	} else if (!strcmp(c, "cat")) {
		FILE *fp = fopen(a, "r");
		char buf[4096];
		size_t n;
		printf("cat %s:%s\n", a, fp ? "" : " (missing)");
		if (fp) {
			while ((n = fread(buf, 1, sizeof(buf), fp)) > 0)
				fwrite(buf, 1, n, stdout);
			fclose(fp);
		}
		return 2;
	} else if (!strcmp(c, "age")) {
		/* age PATH SECS: set the mtime to SECS ago */
		set_age(a, time(NULL) - atoi(b));
		return 3;
	} else if (!strcmp(c, "agetree")) {
		age_to = time(NULL) - atoi(b);
		nftw(a, age_one, 16, FTW_PHYS);
		return 3;
	} else if (!strcmp(c, "setenv")) {
		setenv(a, b, 1);
		return 3;
	} else if (!strcmp(c, "unsetenv")) {
		unsetenv(a);
		return 2;
	} else if (!strcmp(c, "slnew")) {
		rc_stringlist_free(sl);
		sl = rc_stringlist_new();
		held = NULL;
		sl_show();
		return 1;
	} else if (!strcmp(c, "sladd") || !strcmp(c, "sladdu") || !strcmp(c, "sladdnull")) {
		bool u = c[5] == 'u', nul = c[5] == 'n';
		RC_STRING *r;
		errno = 0;
		r = u ? rc_stringlist_addu(sl, a) : rc_stringlist_add(sl, nul ? NULL : a);
		printf("%s: %s", c, r ? "added" : "(null)");
		if (r)
			printf(" last=%s", r == (RC_STRING *)((char *)sl->tqh_last - offsetof(RC_STRING, entries.tqe_next)) ? "yes" : "no");
		else
			printf(" errno=%s", errno ? strerror(errno) : "0");
		printf("\n");
		sl_show();
		return nul ? 1 : 2;
	} else if (!strcmp(c, "sldel")) {
		if (held && held->value && !strcmp(held->value, a))
			held = NULL;	/* may be the node freed */
		B("sldel", rc_stringlist_delete(sl, a));
		sl_show();
		return 2;
	} else if (!strcmp(c, "slfind")) {
		/* slfind V; slfind-null V searches a NULL list */
		RC_STRING *r = rc_stringlist_find(sl, a);
		printf("slfind %s: %s%s%s\n", a, r ? "[" : "", r ? r->value : "(null)", r ? "]" : "");
		return 2;
	} else if (!strcmp(c, "slfind-null")) {
		RC_STRING *r = rc_stringlist_find(NULL, a);
		printf("slfind-null %s: %s\n", a, r ? "found" : "(null)");
		return 2;
	} else if (!strcmp(c, "slhold")) {
		held = rc_stringlist_find(sl, a);
		sl_show();
		return 2;
	} else if (!strcmp(c, "slsplit")) {
		/* slsplit VALUE SEP ("-" VALUE is NULL) */
		rc_stringlist_free(sl);
		held = NULL;
		sl = rc_stringlist_split(strcmp(a, "-") ? a : NULL, b);
		sl_show();
		return 3;
	} else if (!strcmp(c, "slsort")) {
		RC_STRINGLIST *before = sl;
		rc_stringlist_sort(&sl);
		printf("slsort: head %s\n", sl == before ? "same" : "new");
		sl_show();
		return 1;
	} else if (!strcmp(c, "slfree")) {
		rc_stringlist_free(sl);
		sl = NULL;
		held = NULL;
		rc_stringlist_free(NULL);
		sl_show();
		return 1;
	} else if (!strcmp(c, "cfgval")) {
		char *v = rc_config_value(sl, a);
		printf("cfgval %s: %s%s%s", a, v ? "[" : "", v ? v : "(null)", v ? "]" : "");
		if (v) {
			RC_STRING *s;
			for (s = sl->tqh_first; s; s = s->entries.tqe_next)
				if (v > s->value && v <= s->value + strlen(s->value))
					printf(" in [%s]", s->value);
		}
		printf("\n");
		return 2;
	} else if (!strcmp(c, "environ")) {
		printf("rc_environ_fd: %s\n", rc_environ_fd ? "set" : "(null)");
		rc_environ_fd = stdout;
		printf("rc_environ_fd: %s\n", rc_environ_fd == stdout ? "stdout" : "?");
		rc_environ_fd = NULL;
		return 1;
	}
	printf("unknown command %s\n", c);
	return 1;
}

int main(int argc, char **argv)
{
	int i = 1;

	while (i < argc) {
		if (!strcmp(argv[i], "--")) {
			i++;
			continue;
		}
		i += run(argc - i, argv + i);
	}
	fflush(stdout);
	return 0;
}

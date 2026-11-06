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
#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <rc.h>

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

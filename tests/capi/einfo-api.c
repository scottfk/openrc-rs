/*
 * einfo-api.c - exercise every libeinfo.so.1 export, for differential
 * testing of openrc-rs's C ABI against the C reference library.
 *
 * Interleaves the program's own stdio output with libeinfo's, prints every
 * return value, and finishes via eerrorx/ewarnx when asked (argv[1]).
 *
 * Copyright (c) 2026 scottfk; BSD-2-Clause, see LICENSE.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <einfo.h>

#define R(expr) printf("[%s = %d]\n", #expr, (expr))

int main(int argc, char **argv)
{
	const char *mode = argc > 1 ? argv[1] : "";
	int i;

	printf("start (buffered, no newline yet) ");
	R(einfo("hello %s %d", "world", 42));
	R(einfon("no newline %c", 'x'));
	R(einfo("after einfon"));
	R(ewarn("warn %05.1f", 3.14159));
	R(ewarnn("warnn"));
	R(eerror("error %s", "bad"));
	R(eerrorn("errorn"));
	R(einfo(NULL));
	R(einfo("%s", ""));
	R(ebegin("Starting %s", "thing"));
	R(eend(0, NULL));
	R(ebegin("Failing"));
	R(eend(1, "it failed: %d", 7));
	R(eend(3, ""));
	R(ewend(1, "careful %s", "now"));
	R(ewend(0, "ignored"));
	R(einfov("verbose only"));
	R(einfovn("verbose n"));
	R(ewarnv("verbose warn"));
	R(ewarnvn("verbose warnn"));
	R(ebeginv("verbose begin"));
	R(eendv(1, "verbose end"));
	R(ewendv(1, "verbose wend"));
	R(eendv(0, NULL));
	R(einfo("embedded %c nul", 0));

	for (i = 0; i < 3; i++) {
		eindent();
		R(einfo("indent level %d (%s)", i + 1, getenv("EINFO_INDENT")));
	}
	for (i = 0; i < 4; i++) {
		eoutdent();
		R(einfo("outdent %d (%s)", i, getenv("EINFO_INDENT") ? getenv("EINFO_INDENT") : "unset"));
	}
	eindentv();
	R(einfo("indentv (%s)", getenv("EINFO_INDENT") ? getenv("EINFO_INDENT") : "unset"));
	eoutdentv();

	eprefix("pfx");
	R(einfo("prefixed"));
	R(ewarn("prefixed warn"));
	eprefix(NULL);
	R(einfo("unprefixed"));

	printf("colours:");
	for (i = 0; i <= 7; i++)
		printf(" %d=[%s]", i, ecolor((ECOLOR)i));
	printf("\n");

	ebracket(10, ECOLOR_HILITE, "brk");
	ebracket(0, ECOLOR_GOOD, NULL);
	ebracket(100, (ECOLOR)99, "odd");
	elog(3, NULL);

	printf("lastcmd=%s\n", getenv("EINFO_LASTCMD") ? getenv("EINFO_LASTCMD") : "unset");
	printf("end without newline");

	if (strcmp(mode, "eerrorx") == 0)
		eerrorx("fatal %s", "error");
	if (strcmp(mode, "ewarnx") == 0)
		ewarnx("fatal %s", "warning");
	if (strcmp(mode, "eerrorx-null") == 0)
		eerrorx(NULL);
	return 0;
}

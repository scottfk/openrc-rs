/*
 * einfo-shim.c - variadic entry points of libeinfo.so.1 for openrc-rs.
 *
 * Stable Rust cannot define C-variadic functions, so the printf-style API
 * lives here: each function formats its arguments with vsnprintf() and
 * hands the result (with its length, so embedded NULs survive) to the
 * Rust implementation.  Output comes back through rs_einfo_cwrite() so it
 * goes through the calling program's own stdio buffers.
 *
 * Copyright (c) 2026 scottfk; BSD-2-Clause, see LICENSE.
 */

#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

#include "einfo.h"

/* Implemented in Rust (crates/capi-einfo/src/lib.rs). */
int rs_einfo_msg(int which, const char *msg, size_t len);
int rs_einfo_end(int which, int retval, const char *msg, size_t len);
void rs_einfo_exit(int which, const char *msg, size_t len) __attribute__((__noreturn__));
void rs_elog(int level, const char *msg, size_t len);

/* Keep in sync with the Rust side. */
enum {
	M_EINFON, M_EWARNN, M_EERRORN, M_EINFO, M_EWARN, M_EERROR,
	M_EINFOVN, M_EWARNVN, M_EINFOV, M_EWARNV, M_EBEGINV, M_EBEGIN,
};
enum { E_EEND, E_EWEND, E_EENDV, E_EWENDV };
enum { X_EWARNX, X_EERRORX };

/* Called from Rust: write through this program's stdio. */
void rs_einfo_cwrite(int stream, const char *buf, size_t len)
{
	fwrite(buf, 1, len, stream == 2 ? stderr : stdout);
}

/* NULL fmt stays NULL (the Rust side treats it as "no message"). */
static char *format(const char *fmt, va_list ap, size_t *len)
{
	va_list cp;
	char *buf;
	int n;

	*len = 0;
	if (!fmt)
		return NULL;
	va_copy(cp, ap);
	n = vsnprintf(NULL, 0, fmt, cp);
	va_end(cp);
	if (n < 0)
		n = 0;
	buf = malloc((size_t)n + 1);
	if (!buf) {
		fprintf(stderr, "out of memory\n");
		exit(1);
	}
	if (n > 0)
		vsnprintf(buf, (size_t)n + 1, fmt, ap);
	buf[n] = '\0';
	*len = (size_t)n;
	return buf;
}

#define MSG_FN(name, id)						\
int name(const char *EINFO_RESTRICT fmt, ...)				\
{									\
	va_list ap;							\
	size_t len;							\
	char *msg;							\
	int r;								\
									\
	va_start(ap, fmt);						\
	msg = format(fmt, ap, &len);					\
	va_end(ap);							\
	r = rs_einfo_msg(id, msg, len);					\
	free(msg);							\
	return r;							\
}

#define END_FN(name, id)						\
int name(int retval, const char *EINFO_RESTRICT fmt, ...)		\
{									\
	va_list ap;							\
	size_t len;							\
	char *msg;							\
	int r;								\
									\
	va_start(ap, fmt);						\
	msg = format(fmt, ap, &len);					\
	va_end(ap);							\
	r = rs_einfo_end(id, retval, msg, len);				\
	free(msg);							\
	return r;							\
}

#define EXIT_FN(name, id)						\
void name(const char *EINFO_RESTRICT fmt, ...)				\
{									\
	va_list ap;							\
	size_t len;							\
	char *msg;							\
									\
	va_start(ap, fmt);						\
	msg = format(fmt, ap, &len);					\
	va_end(ap);							\
	rs_einfo_exit(id, msg, len);					\
}

MSG_FN(einfon, M_EINFON)
MSG_FN(ewarnn, M_EWARNN)
MSG_FN(eerrorn, M_EERRORN)
MSG_FN(einfo, M_EINFO)
MSG_FN(ewarn, M_EWARN)
MSG_FN(eerror, M_EERROR)
MSG_FN(einfovn, M_EINFOVN)
MSG_FN(ewarnvn, M_EWARNVN)
MSG_FN(einfov, M_EINFOV)
MSG_FN(ewarnv, M_EWARNV)
MSG_FN(ebeginv, M_EBEGINV)
MSG_FN(ebegin, M_EBEGIN)

END_FN(eend, E_EEND)
END_FN(ewend, E_EWEND)
END_FN(eendv, E_EENDV)
END_FN(ewendv, E_EWENDV)

EXIT_FN(ewarnx, X_EWARNX)
EXIT_FN(eerrorx, X_EERRORX)

void elog(int level, const char *EINFO_RESTRICT fmt, ...)
{
	va_list ap;
	size_t len;
	char *msg;

	va_start(ap, fmt);
	msg = format(fmt, ap, &len);
	va_end(ap);
	rs_elog(level, msg, len);
	free(msg);
}

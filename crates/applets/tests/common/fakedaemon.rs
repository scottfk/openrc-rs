//! A fake daemon for the start-stop-daemon (and later supervise-daemon)
//! tests, built with cc.  It's a real binary so `--exec` matching (by
//! `/proc/<pid>/exe`) works.
//!
//! On start it appends a report to `$FAKE_OUT`: its argv, the environment
//! (sorted), cwd, ids, umask, nice, I/O priority, OOM score, scheduler,
//! capability sets, securebits, no_new_privs, where fds 0-2 point, and
//! whether it's a session leader.  It writes `out` to stdout and `err` to
//! stderr.  Then `$FAKE_MODE` says what it does:
//!
//! - `run` (default): wait to be killed.
//! - `fork`: fork; the child writes its pid to `$FAKE_PIDFILE` (if set)
//!   and waits; the parent exits 0 (a classic self-daemonizing daemon).
//! - `exit:N`: exit with status N.
//! - `stubborn`: ignore SIGTERM, then wait.
//! - `notify-fd:N`: write "ready\n" to fd N, then wait.
//! - `notify-socket`: send READY=1 to `$NOTIFY_SOCKET`, then wait.
//! - `fork-die`: like `fork`, but the child exits after 50ms.
//! - `notify-partial`: write "not ready" (no newline) to fd 4, exit after
//!   0.2s.
//! - `slowexit`: exit 0 after 0.5s.
//! - `stubborn-slowexit`: ignore SIGTERM and SIGHUP, exit 0 after 0.5s.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const SRC: &str = r#"
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <grp.h>
#include <limits.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <unistd.h>

extern char **environ;

static int cmp(const void *a, const void *b) { return strcmp(*(char **)a, *(char **)b); }

static void report(FILE *f, int argc, char **argv)
{
	char buf[PATH_MAX], line[512];
	fprintf(f, "argv:");
	for (int i = 0; i < argc; i++)
		fprintf(f, " [%s]", argv[i]);
	fprintf(f, "\n");
	int n = 0;
	while (environ[n]) n++;
	char **e = malloc(sizeof(char *) * (n + 1));
	memcpy(e, environ, sizeof(char *) * (n + 1));
	qsort(e, n, sizeof(char *), cmp);
	for (int i = 0; i < n; i++)
		fprintf(f, "env: %s\n", e[i]);
	fprintf(f, "cwd: %s\n", getcwd(buf, sizeof(buf)) ? buf : "?");
	gid_t groups[64];
	int ng = getgroups(64, groups);
	fprintf(f, "ids: uid=%d gid=%d groups=%d\n", (int)getuid(), (int)getgid(), ng);
	mode_t m = umask(0); umask(m);
	fprintf(f, "umask: %03o\n", (unsigned)m);
	errno = 0;
	fprintf(f, "nice: %d\n", getpriority(PRIO_PROCESS, 0));
	fprintf(f, "ioprio: %ld\n", syscall(SYS_ioprio_get, 1, 0));
	FILE *o = fopen("/proc/self/oom_score_adj", "r");
	if (o) { if (fgets(line, sizeof(line), o)) fprintf(f, "oom: %s", line); fclose(o); }
	struct sched_param sp;
	sched_getparam(0, &sp);
	fprintf(f, "sched: %d %d\n", sched_getscheduler(0), sp.sched_priority);
	FILE *s = fopen("/proc/self/status", "r");
	if (s) {
		while (fgets(line, sizeof(line), s))
			if (!strncmp(line, "Cap", 3) || !strncmp(line, "NoNewPrivs", 10))
				fprintf(f, "%s", line);
		fclose(s);
	}
	fprintf(f, "secbits: %d\n", prctl(PR_GET_SECUREBITS, 0, 0, 0, 0));
	for (int fd = 0; fd < 3; fd++) {
		char p[64];
		snprintf(p, sizeof(p), "/proc/self/fd/%d", fd);
		ssize_t l = readlink(p, buf, sizeof(buf) - 1);
		if (l < 0) strcpy(buf, "closed");
		else buf[l] = 0;
		/* pipe/socket inode numbers differ between runs */
		if (!strncmp(buf, "pipe:", 5)) strcpy(buf, "pipe");
		if (!strncmp(buf, "socket:", 7)) strcpy(buf, "socket");
		fprintf(f, "fd%d: %s\n", fd, buf);
	}
	fprintf(f, "session-leader: %d\n", getsid(0) == getpid());
}

static void wait_forever(void) { for (;;) pause(); }

int main(int argc, char **argv)
{
	const char *out = getenv("FAKE_OUT");
	const char *mode = getenv("FAKE_MODE");
	if (!mode) mode = "run";
	if (out) {
		FILE *f = fopen(out, "a");
		if (f) { report(f, argc, argv); fclose(f); }
	}
	printf("out\n"); fflush(stdout);
	fprintf(stderr, "err\n");
	if (!strcmp(mode, "fork")) {
		pid_t p = fork();
		if (p == 0) {
			const char *pf = getenv("FAKE_PIDFILE");
			if (pf) { FILE *f = fopen(pf, "w"); if (f) { fprintf(f, "%d\n", getpid()); fclose(f); } }
			wait_forever();
		}
		return 0;
	}
	if (!strcmp(mode, "fork-die")) {
		if (fork() == 0) { usleep(50000); return 0; }
		return 0;
	}
	if (!strcmp(mode, "notify-partial")) {
		if (write(4, "not ready", 9) < 0) return 9;
		usleep(200000);
		return 0;
	}
	if (!strncmp(mode, "exit:", 5))
		return atoi(mode + 5);
	if (!strcmp(mode, "slowexit")) { usleep(500000); return 0; }
	if (!strcmp(mode, "stubborn-slowexit")) {
		signal(SIGTERM, SIG_IGN); signal(SIGHUP, SIG_IGN);
		usleep(500000); return 0;
	}
	if (!strcmp(mode, "stubborn")) { signal(SIGTERM, SIG_IGN); wait_forever(); }
	if (!strncmp(mode, "notify-fd:", 10)) {
		int fd = atoi(mode + 10);
		if (write(fd, "ready\n", 6) < 0) return 9;
		wait_forever();
	}
	if (!strcmp(mode, "notify-socket")) {
		const char *path = getenv("NOTIFY_SOCKET");
		int s = socket(AF_UNIX, SOCK_DGRAM, 0);
		struct sockaddr_un a = { .sun_family = AF_UNIX };
		if (path) strncpy(a.sun_path, path, sizeof(a.sun_path) - 1);
		sendto(s, "READY=1", 7, 0, (struct sockaddr *)&a, sizeof(a));
		wait_forever();
	}
	wait_forever();
}
"#;

/// The built fake daemon (one per test binary).
pub fn fakedaemon() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(env!("CARGO_CRATE_NAME"));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("fakedaemon.c");
        fs::write(&src, SRC).unwrap();
        let bin = dir.join("fakedaemon");
        let ok = Command::new("cc").args(["-O2", "-o"]).arg(&bin).arg(&src).status().unwrap();
        assert!(ok.success(), "building the fake daemon failed");
        bin
    })
}

/*
 * Deterministic carpet coverage for the StarryOS POSIX message queue
 * subsystem. Complements the LTP / Open POSIX conformance suites with the
 * edge cases those suites under-exercise on a single-user kernel: strict
 * priority ordering (across and within priority), full/empty blocking with
 * EAGAIN / ETIMEDOUT, EMSGSIZE / EEXIST / ENOENT / EINVAL, mq_notify
 * SIGEV_SIGNAL / SIGEV_NONE / EBUSY, cross-process fork IPC, and the
 * /dev/mqueue listing and QSIZE/NOTIFY status format.
 *
 * Each check prints "ok N - <desc>" or "not ok N - <desc>"; the program ends
 * with an aggregate line and "MQ_OK=<pass>/<total>" plus "TEST PASSED" only
 * when every check passed.
 */

#include <errno.h>
#include <fcntl.h>
#include <mqueue.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int g_count;
static int g_pass;

static void ok(int cond, const char *desc)
{
	g_count++;
	if (cond) {
		g_pass++;
		printf("ok %d - %s\n", g_count, desc);
	} else {
		printf("not ok %d - %s (errno=%d %s)\n", g_count, desc, errno,
		       strerror(errno));
	}
}

static volatile sig_atomic_t g_notified;

static void notify_handler(int sig)
{
	(void)sig;
	g_notified = 1;
}

/* SIGEV_THREAD callback: musl runs this on its helper thread after the kernel
 * pushes the notification cookie over the netlink socket. */
static void notify_thread_fn(union sigval sv)
{
	(void)sv;
	g_notified = 1;
}

/* Absolute CLOCK_REALTIME deadline `ms` milliseconds from now. */
static struct timespec deadline_in(long ms)
{
	struct timespec ts;
	clock_gettime(CLOCK_REALTIME, &ts);
	ts.tv_nsec += (ms % 1000) * 1000000L;
	ts.tv_sec += ms / 1000;
	if (ts.tv_nsec >= 1000000000L) {
		ts.tv_sec += 1;
		ts.tv_nsec -= 1000000000L;
	}
	return ts;
}

int main(void)
{
	const char *name = "/mq_carpet";
	struct mq_attr attr;
	mqd_t q;

	mq_unlink(name); /* clean slate */

	/* ---- mq_open: create with attr, then O_EXCL rejects re-create ---- */
	memset(&attr, 0, sizeof(attr));
	attr.mq_maxmsg = 4;
	attr.mq_msgsize = 32;
	q = mq_open(name, O_CREAT | O_RDWR, 0644, &attr);
	ok(q != (mqd_t)-1, "mq_open O_CREAT|O_RDWR with attr");

	mqd_t dup = mq_open(name, O_CREAT | O_EXCL | O_RDWR, 0644, NULL);
	ok(dup == (mqd_t)-1 && errno == EEXIST, "mq_open O_CREAT|O_EXCL -> EEXIST");

	/* ---- mq_getattr reflects the requested attributes ---- */
	/* Zero-initialized so that later uses (the `nb = got` copy at the
	 * O_NONBLOCK test) start from defined bytes even if the initial
	 * mq_open/mq_getattr above failed and never populated it. */
	struct mq_attr got = {0};
	ok(mq_getattr(q, &got) == 0 && got.mq_maxmsg == 4 && got.mq_msgsize == 32 &&
		   got.mq_curmsgs == 0,
	   "mq_getattr returns maxmsg/msgsize/curmsgs");

	/* ---- open non-existent without O_CREAT -> ENOENT ---- */
	mqd_t none = mq_open("/mq_carpet_absent", O_RDWR, 0644, NULL);
	ok(none == (mqd_t)-1 && errno == ENOENT, "mq_open missing -> ENOENT");

	/* ---- name with an embedded '/' is invalid: mqueue names are a single
	 * component. musl's mq_open strips only the leading '/' and passes the
	 * rest through (unlike glibc it does not require a leading '/'), so the
	 * kernel is what must reject the interior slash -> EINVAL. ---- */
	mqd_t bad = mq_open("/mq_carpet/bad", O_CREAT | O_RDWR, 0644, NULL);
	ok(bad == (mqd_t)-1 && errno == EINVAL, "mq_open name with embedded slash -> EINVAL");

	/* ---- privileged caller may exceed the soft msg_max/msgsize_max
	 * ceilings up to the hard limits (Linux CAP_SYS_RESOURCE). StarryOS
	 * runs as root, so maxmsg=20 / msgsize=16384 must be accepted. ---- */
	{
		struct mq_attr big_attr;
		memset(&big_attr, 0, sizeof(big_attr));
		big_attr.mq_maxmsg = 20;
		big_attr.mq_msgsize = 16384;
		mqd_t bq = mq_open("/mq_carpet_big", O_CREAT | O_EXCL | O_RDWR, 0644,
				   &big_attr);
		struct mq_attr bg;
		int good = bq != (mqd_t)-1 && mq_getattr(bq, &bg) == 0 &&
			   bg.mq_maxmsg == 20 && bg.mq_msgsize == 16384;
		ok(good, "privileged mq_open exceeds soft ceilings (maxmsg=20)");
		if (bq != (mqd_t)-1)
			mq_close(bq);
		mq_unlink("/mq_carpet_big");
	}

	/* ---- priority ordering: highest priority dequeued first ---- */
	ok(mq_send(q, "lo", 2, 1) == 0, "mq_send prio 1");
	ok(mq_send(q, "hi", 2, 9) == 0, "mq_send prio 9");
	ok(mq_send(q, "mid", 3, 5) == 0, "mq_send prio 5");

	char buf[64];
	unsigned int prio;
	ssize_t n;

	n = mq_receive(q, buf, sizeof(buf), &prio);
	ok(n == 2 && prio == 9 && memcmp(buf, "hi", 2) == 0,
	   "mq_receive returns highest priority first");
	n = mq_receive(q, buf, sizeof(buf), &prio);
	ok(n == 3 && prio == 5 && memcmp(buf, "mid", 3) == 0,
	   "mq_receive returns middle priority second");
	n = mq_receive(q, buf, sizeof(buf), &prio);
	ok(n == 2 && prio == 1 && memcmp(buf, "lo", 2) == 0,
	   "mq_receive returns lowest priority last");

	/* ---- FIFO within the same priority ---- */
	mq_send(q, "A", 1, 3);
	mq_send(q, "B", 1, 3);
	mq_send(q, "C", 1, 3);
	mq_receive(q, buf, sizeof(buf), &prio);
	int fifo = (buf[0] == 'A');
	mq_receive(q, buf, sizeof(buf), &prio);
	fifo = fifo && (buf[0] == 'B');
	mq_receive(q, buf, sizeof(buf), &prio);
	fifo = fifo && (buf[0] == 'C');
	ok(fifo, "FIFO order preserved within one priority");

	/* ---- curmsgs tracks queue depth ---- */
	mq_send(q, "x", 1, 0);
	mq_send(q, "y", 1, 0);
	mq_getattr(q, &got);
	ok(got.mq_curmsgs == 2, "mq_getattr curmsgs counts queued messages");
	mq_receive(q, buf, sizeof(buf), &prio);
	mq_receive(q, buf, sizeof(buf), &prio);

	/* ---- EMSGSIZE: payload larger than mq_msgsize ---- */
	char big[64];
	memset(big, 'z', sizeof(big));
	ok(mq_send(q, big, 40, 0) == -1 && errno == EMSGSIZE,
	   "mq_send oversize -> EMSGSIZE");

	/* ---- O_NONBLOCK send on a full queue -> EAGAIN ---- */
	struct mq_attr nb = got;
	nb.mq_flags = O_NONBLOCK;
	mq_setattr(q, &nb, NULL);
	for (int i = 0; i < 4; i++)
		mq_send(q, "f", 1, 0);
	ok(mq_send(q, "f", 1, 0) == -1 && errno == EAGAIN,
	   "mq_send full nonblocking -> EAGAIN");

	/* ---- O_NONBLOCK receive on an empty queue -> EAGAIN ---- */
	for (int i = 0; i < 4; i++)
		mq_receive(q, buf, sizeof(buf), &prio);
	ok(mq_receive(q, buf, sizeof(buf), &prio) == -1 && errno == EAGAIN,
	   "mq_receive empty nonblocking -> EAGAIN");

	/* ---- clear O_NONBLOCK, verify mq_setattr/mq_getattr round-trip ---- */
	nb.mq_flags = 0;
	mq_setattr(q, &nb, NULL);
	mq_getattr(q, &got);
	ok((got.mq_flags & O_NONBLOCK) == 0, "mq_setattr clears O_NONBLOCK");

	/* ---- mq_timedreceive on empty queue -> ETIMEDOUT ---- */
	{
		struct timespec ts = deadline_in(120);
		n = mq_timedreceive(q, buf, sizeof(buf), &prio, &ts);
		ok(n == -1 && errno == ETIMEDOUT,
		   "mq_timedreceive empty -> ETIMEDOUT");
	}

	/* ---- mq_timedsend on full queue -> ETIMEDOUT ---- */
	for (int i = 0; i < 4; i++)
		mq_send(q, "f", 1, 0);
	{
		struct timespec ts = deadline_in(120);
		ok(mq_timedsend(q, "f", 1, 0, &ts) == -1 && errno == ETIMEDOUT,
		   "mq_timedsend full -> ETIMEDOUT");
	}
	for (int i = 0; i < 4; i++)
		mq_receive(q, buf, sizeof(buf), &prio);

	/* ---- mq_notify: SIGEV_SIGNAL fires on empty -> non-empty ---- */
	struct sigaction sa;
	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = notify_handler;
	sigaction(SIGUSR1, &sa, NULL);

	struct sigevent sev;
	memset(&sev, 0, sizeof(sev));
	sev.sigev_notify = SIGEV_SIGNAL;
	sev.sigev_signo = SIGUSR1;
	ok(mq_notify(q, &sev) == 0, "mq_notify SIGEV_SIGNAL registers");

	/* ---- second registration while one is active -> EBUSY ---- */
	ok(mq_notify(q, &sev) == -1 && errno == EBUSY,
	   "mq_notify double register -> EBUSY");

	g_notified = 0;
	mq_send(q, "ping", 4, 0);
	for (int i = 0; i < 100 && !g_notified; i++)
		usleep(1000);
	ok(g_notified == 1, "mq_notify delivers signal on arrival");
	mq_receive(q, buf, sizeof(buf), &prio);

	/* registration is single-shot: re-register succeeds after delivery */
	ok(mq_notify(q, &sev) == 0, "mq_notify re-register after delivery");
	/* explicit unregister */
	ok(mq_notify(q, NULL) == 0, "mq_notify NULL unregisters");

	/* ---- SIGEV_NONE registers without a signal ---- */
	memset(&sev, 0, sizeof(sev));
	sev.sigev_notify = SIGEV_NONE;
	ok(mq_notify(q, &sev) == 0, "mq_notify SIGEV_NONE registers");
	mq_notify(q, NULL);

	/* ---- cross-process IPC: child sends, parent receives ---- */
	pid_t pid = fork();
	if (pid == 0) {
		mqd_t cq = mq_open(name, O_WRONLY, 0644, NULL);
		mq_send(cq, "from-child", 10, 7);
		mq_close(cq);
		_exit(0);
	}
	{
		struct timespec ts = deadline_in(2000);
		n = mq_timedreceive(q, buf, sizeof(buf), &prio, &ts);
		ok(n == 10 && prio == 7 && memcmp(buf, "from-child", 10) == 0,
		   "cross-process fork IPC send/receive");
	}
	waitpid(pid, NULL, 0);

	/* ---- /dev/mqueue lists the queue with QSIZE/NOTIFY status ---- */
	mq_send(q, "resident", 8, 0);
	{
		FILE *f = fopen("/dev/mqueue/mq_carpet", "r");
		int have_line = 0;
		if (f) {
			char line[128];
			if (fgets(line, sizeof(line), f))
				have_line = strstr(line, "QSIZE:") != NULL &&
					    strstr(line, "NOTIFY:") != NULL;
			fclose(f);
		}
		ok(have_line, "/dev/mqueue/<name> reports QSIZE/NOTIFY status");
	}
	mq_receive(q, buf, sizeof(buf), &prio);

	/* ---- opening an existing queue with the invalid access mode
	 * O_RDWR|O_WRONLY (the 0b11 O_ACCMODE value) is rejected with EINVAL,
	 * matching Linux prepare_open(). ---- */
	{
		mqd_t rw = mq_open(name, O_RDWR | O_WRONLY, 0644, NULL);
		ok(rw == (mqd_t)-1 && errno == EINVAL,
		   "mq_open existing with O_RDWR|O_WRONLY -> EINVAL");
		if (rw != (mqd_t)-1)
			mq_close(rw);
	}

	/* ---- mq_notify SIGEV_SIGNAL validates the signal number at
	 * registration time: an out-of-range signo is EINVAL (Linux
	 * valid_signal), while signo 0 is accepted (registers, never delivers). */
	{
		struct sigevent s;
		memset(&s, 0, sizeof(s));
		s.sigev_notify = SIGEV_SIGNAL;
		s.sigev_signo = 12345; /* > _NSIG (64) */
		ok(mq_notify(q, &s) == -1 && errno == EINVAL,
		   "mq_notify out-of-range sigev_signo -> EINVAL");
		s.sigev_signo = 0; /* valid_signal(0) is true */
		ok(mq_notify(q, &s) == 0, "mq_notify sigev_signo 0 registers");
		mq_notify(q, NULL);
	}

	/* ---- mq_notify(NULL) from a non-owner is a silent no-op: only the
	 * registering task's tgid may clear the registration (Linux
	 * do_mq_notify gates on notify_owner). The child registers, the parent's
	 * NULL must not clear it, so the child still receives the delivery. ---- */
	{
		struct sigevent s;
		memset(&s, 0, sizeof(s));
		s.sigev_notify = SIGEV_SIGNAL;
		s.sigev_signo = SIGUSR1;
		int pipefd[2];
		int have_pipe = pipe(pipefd) == 0;
		pid_t cpid = fork();
		if (cpid == 0) {
			mqd_t cq = mq_open(name, O_RDWR, 0644, NULL);
			struct sigaction ca;
			memset(&ca, 0, sizeof(ca));
			ca.sa_handler = notify_handler;
			sigaction(SIGUSR1, &ca, NULL);
			g_notified = 0;
			mq_notify(cq, &s); /* child owns the registration */
			if (have_pipe)
				{ ssize_t _r = write(pipefd[1], "r", 1); (void)_r; }
			for (int i = 0; i < 500 && !g_notified; i++)
				usleep(1000);
			_exit(g_notified ? 0 : 2);
		}
		char c;
		if (have_pipe)
			(void)!read(pipefd[0], &c, 1); /* wait until child registered */
		/* parent (non-owner) attempts to clear: must be a silent no-op */
		int parent_clear = mq_notify(q, NULL);
		mq_send(q, "hey", 3, 0); /* empty->non-empty fires the child's notify */
		int status = 0;
		waitpid(cpid, &status, 0);
		mq_receive(q, buf, sizeof(buf), &prio);
		ok(parent_clear == 0 && WIFEXITED(status) && WEXITSTATUS(status) == 0,
		   "mq_notify(NULL) from non-owner is a no-op; owner still notified");
		if (have_pipe) {
			close(pipefd[0]);
			close(pipefd[1]);
		}
	}

	/* ---- a privileged (CAP_SYS_RESOURCE) caller may create more than
	 * mq_queues_max queues; StarryOS runs as root, so creating past the cap
	 * must succeed (Linux mqueue_create_attr bypass). Use minimal queues
	 * (maxmsg=1/msgsize=1, ~97 bytes of RLIMIT_MSGQUEUE charge each) so the
	 * 260-queue run stays well under the per-user byte limit (819200) and this
	 * check isolates the *count* bypass from the *byte* accounting, exactly as
	 * on real Linux where root bypasses queues_max but is still bounded by
	 * RLIMIT_MSGQUEUE. ---- */
	{
		enum { NQ = 260 }; /* > MQ_QUEUES_MAX (256) */
		static mqd_t qs[NQ];
		struct mq_attr tiny;
		memset(&tiny, 0, sizeof(tiny));
		tiny.mq_maxmsg = 1;
		tiny.mq_msgsize = 1;
		char nm[32];
		int made = 0;
		for (int i = 0; i < NQ; i++) {
			snprintf(nm, sizeof(nm), "/mqcap_%d", i);
			qs[i] = mq_open(nm, O_CREAT | O_RDWR, 0644, &tiny);
			if (qs[i] != (mqd_t)-1)
				made++;
		}
		ok(made == NQ,
		   "privileged caller exceeds mq_queues_max (CAP_SYS_RESOURCE bypass)");
		for (int i = 0; i < NQ; i++) {
			snprintf(nm, sizeof(nm), "/mqcap_%d", i);
			if (qs[i] != (mqd_t)-1)
				mq_close(qs[i]);
			mq_unlink(nm);
		}
	}

	/* ==== GAP A: RLIMIT_MSGQUEUE accounting ==================================
	 * Linux charges each queue's mq_bytes (maxmsg*(sizeof(msg_msg)+msgsize) +
	 * tree overhead) against the creator's RLIMIT_MSGQUEUE; a create that would
	 * push the user past the soft limit fails with EMFILE (ipc/mqueue.c:373).
	 * Verify: (1) the default limit is MQ_BYTES_MAX (819200), (2) lowering the
	 * soft limit makes an otherwise-legal large queue fail with EMFILE, and
	 * (3) a queue whose bytes fit still succeeds. */
	{
		struct rlimit rl;
		int have = getrlimit(RLIMIT_MSGQUEUE, &rl) == 0;
		ok(have && rl.rlim_cur == 819200,
		   "RLIMIT_MSGQUEUE default soft limit is MQ_BYTES_MAX (819200)");

		/* Lower the soft limit to a tiny value. A maxmsg=10/msgsize=8192 queue
		 * needs >81920 bytes of payload accounting alone, far above 4096, so
		 * the create must be refused with EMFILE. */
		struct rlimit small = rl;
		small.rlim_cur = 4096;
		int set_ok = setrlimit(RLIMIT_MSGQUEUE, &small) == 0;
		struct mq_attr over;
		memset(&over, 0, sizeof(over));
		over.mq_maxmsg = 10;
		over.mq_msgsize = 8192;
		mq_unlink("/mq_rlim_over");
		mqd_t oq = mq_open("/mq_rlim_over", O_CREAT | O_EXCL | O_RDWR, 0644, &over);
		ok(set_ok && oq == (mqd_t)-1 && errno == EMFILE,
		   "mq_open past RLIMIT_MSGQUEUE -> EMFILE");
		if (oq != (mqd_t)-1) {
			mq_close(oq);
			mq_unlink("/mq_rlim_over");
		}

		/* A queue small enough to fit under 4096 bytes of charge still opens:
		 * maxmsg=1/msgsize=64 -> 64 + tree(48+48) = 160 bytes << 4096. */
		struct mq_attr fit;
		memset(&fit, 0, sizeof(fit));
		fit.mq_maxmsg = 1;
		fit.mq_msgsize = 64;
		mq_unlink("/mq_rlim_fit");
		mqd_t fq = mq_open("/mq_rlim_fit", O_CREAT | O_EXCL | O_RDWR, 0644, &fit);
		ok(fq != (mqd_t)-1, "mq_open within RLIMIT_MSGQUEUE succeeds");
		if (fq != (mqd_t)-1) {
			mq_close(fq);
			mq_unlink("/mq_rlim_fit");
		}

		/* Restore the original limit so later checks are unaffected. */
		setrlimit(RLIMIT_MSGQUEUE, &rl);
	}

	/* ==== GAP B: /proc/sys/fs/mqueue tunables ================================
	 * Linux exposes msg_max/msgsize_max/queues_max/msg_default/msgsize_default
	 * under /proc/sys/fs/mqueue (ipc/mq_sysctl.c). Verify they read back and
	 * that a lowered msg_max is honored by mq_open: an unprivileged-ceiling
	 * attribute above the new msg_max must be rejected with EINVAL, and an
	 * attr-less create must clamp maxmsg to the lowered msg_max. */
	{
		char buf2[64];
		long msg_max_val = -1;
		FILE *f = fopen("/proc/sys/fs/mqueue/msg_max", "r");
		if (f) {
			if (fgets(buf2, sizeof(buf2), f))
				msg_max_val = strtol(buf2, NULL, 10);
			fclose(f);
		}
		ok(msg_max_val == 10, "/proc/sys/fs/mqueue/msg_max reads back default 10");

		/* All five tunables must be present and readable. */
		int all_present = 1;
		const char *names[] = {"msg_max", "msgsize_max", "queues_max",
				       "msg_default", "msgsize_default"};
		for (unsigned i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
			char path[64];
			snprintf(path, sizeof(path), "/proc/sys/fs/mqueue/%s", names[i]);
			FILE *ff = fopen(path, "r");
			if (ff) {
				all_present = all_present && (fgets(buf2, sizeof(buf2), ff) != NULL);
				fclose(ff);
			} else {
				all_present = 0;
			}
		}
		ok(all_present, "/proc/sys/fs/mqueue/* all readable");

		/* Lower msg_max to 5 and confirm mq_open honors it. */
		int wrote = 0;
		f = fopen("/proc/sys/fs/mqueue/msg_max", "w");
		if (f) {
			wrote = fputs("5\n", f) >= 0;
			fclose(f);
		}
		/* Read-back reflects the write. */
		long after = -1;
		f = fopen("/proc/sys/fs/mqueue/msg_max", "r");
		if (f) {
			if (fgets(buf2, sizeof(buf2), f))
				after = strtol(buf2, NULL, 10);
			fclose(f);
		}
		ok(wrote && after == 5, "writing /proc/sys/fs/mqueue/msg_max updates it");

		/* An unprivileged-ceiling attribute above the new msg_max is rejected;
		 * StarryOS runs as root (CAP_SYS_RESOURCE) so it is still bounded only
		 * by the hard limit — but an attr-less create clamps to msg_max. The
		 * portable, capability-independent check is the attr-less default: a
		 * queue created with no attr must now have maxmsg <= 5. */
		mq_unlink("/mq_tune_default");
		mqd_t tq = mq_open("/mq_tune_default", O_CREAT | O_EXCL | O_RDWR, 0644, NULL);
		struct mq_attr ta;
		int clamped = tq != (mqd_t)-1 && mq_getattr(tq, &ta) == 0 && ta.mq_maxmsg == 5;
		ok(clamped, "lowered msg_max clamps attr-less mq_open default maxmsg to 5");
		if (tq != (mqd_t)-1) {
			mq_close(tq);
			mq_unlink("/mq_tune_default");
		}

		/* Restore msg_max to 10 so the rest of the suite is unaffected. */
		f = fopen("/proc/sys/fs/mqueue/msg_max", "w");
		if (f) {
			fputs("10\n", f);
			fclose(f);
		}
	}

	/* ==== GAP C: /dev/mqueue inode stat =====================================
	 * Linux stamps the mqueuefs inode with i_mode (S_IFREG | perm), i_uid/i_gid
	 * (creator fsuid/fsgid) and real timestamps. Verify stat("/dev/mqueue/<q>")
	 * reports a regular file with the queue's permission bits, the creator uid,
	 * and a non-zero modification time. */
	{
		struct stat st;
		int have = stat("/dev/mqueue/mq_carpet", &st) == 0;
		ok(have && S_ISREG(st.st_mode) && (st.st_mode & 0777) == 0644,
		   "/dev/mqueue/<name> stat reports S_IFREG with the queue's mode 0644");
		ok(have && st.st_uid == getuid(),
		   "/dev/mqueue/<name> stat reports the creator uid");
		ok(have && st.st_mtime != 0,
		   "/dev/mqueue/<name> stat reports a non-zero mtime");
	}

	/* ==== GAP D: mq_notify SIGEV_THREAD =====================================
	 * musl (like glibc) implements POSIX SIGEV_THREAD for mq_notify by opening
	 * a PF_NETLINK socket and passing its fd through the kernel; the kernel
	 * delivers a cookie over that socket on message arrival and a helper thread
	 * runs the callback. Verify the callback fires on empty->non-empty. */
	{
		g_notified = 0;
		struct sigevent tev;
		memset(&tev, 0, sizeof(tev));
		tev.sigev_notify = SIGEV_THREAD;
		tev.sigev_notify_function = notify_thread_fn;
		tev.sigev_value.sival_int = 0;
		int reg = mq_notify(q, &tev);
		ok(reg == 0, "mq_notify SIGEV_THREAD registers");
		if (reg == 0) {
			mq_send(q, "thr", 3, 0); /* empty->non-empty fires the thread */
			for (int i = 0; i < 500 && !g_notified; i++)
				usleep(1000);
			ok(g_notified == 1, "mq_notify SIGEV_THREAD callback runs on arrival");
			mq_receive(q, buf, sizeof(buf), &prio);
		} else {
			ok(0, "mq_notify SIGEV_THREAD callback runs on arrival");
		}
	}

	/* ---- mq_unlink removes the name; re-open without O_CREAT fails ---- */
	ok(mq_unlink(name) == 0, "mq_unlink succeeds");
	ok(mq_open(name, O_RDWR, 0644, NULL) == (mqd_t)-1 && errno == ENOENT,
	   "mq_open after unlink -> ENOENT");
	ok(mq_unlink(name) == -1 && errno == ENOENT,
	   "mq_unlink twice -> ENOENT");

	mq_close(q);

	/* Print the carpet aggregate only. The suite runner (run-mq-tests.sh) is
	 * the sole authority for the final "TEST PASSED"/"TEST FAILED" verdict,
	 * gating on both this carpet and the Open POSIX conformance cases, so the
	 * carpet must not emit the success token on its own. */
	printf("AGGREGATE PASS=%d/%d\n", g_pass, g_count);
	printf("MQ_OK=%d/%d\n", g_pass, g_count);
	return g_pass == g_count ? 0 : 1;
}

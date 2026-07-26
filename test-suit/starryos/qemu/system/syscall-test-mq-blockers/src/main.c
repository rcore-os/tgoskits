/*
 * mq-blockers.c - Deterministic regressions for four POSIX message-queue
 * defects, each checked against Linux ipc/mqueue.c semantics. Single-core
 * (-smp 1) deterministic: a worker publishes an "armed" flag immediately
 * before its blocking call and the driver waits for it plus a short settle so
 * the worker is genuinely parked in the syscall before the driver acts.
 *
 *   B1  mq_notify lost on two-sends-before-consume.
 *       A receiver parks on an empty queue; a notification is registered; then
 *       TWO sends land before the receiver is rescheduled. Linux hands send #1
 *       straight to the parked receiver (pipelined_send: queue stays empty, no
 *       __do_notify), so send #2 hits an empty queue with NO waiter and fires
 *       __do_notify exactly once (ipc/mqueue.c:1121-1130,786). A count/defer
 *       model that enqueues send #1 instead leaves the queue non-empty for
 *       send #2 and loses the notification entirely -> this test goes RED on it.
 *
 *   B2  mq_open fd-exhaustion must not leak the named queue.
 *       Exhaust the fd table, attempt mq_open(O_CREAT|O_EXCL) of a fresh name:
 *       it must fail (EMFILE) AND leave no queue behind, so once fds are freed
 *       an O_CREAT|O_EXCL open of the same name SUCCEEDS. Linux do_mq_open
 *       reserves the fd first and unwinds the inode on failure; a charge-before-
 *       fd order that forgets to roll back leaves the name (and rlimit charge)
 *       stranded -> the later O_EXCL open would see EEXIST.
 *
 *   B3  writes to /proc/sys/fs/mqueue tunables need privilege.
 *       These sysctls are root-writable only (Linux mq_permissions,
 *       ipc/mq_sysctl.c:92, files 0644 owned by ns-root). An unprivileged
 *       writer must get EPERM; reads stay open. Verified via a setuid child.
 *
 *   B4  /dev/mqueue/<name> st_size is the fixed FILENT_SIZE (80).
 *       Linux mqueue_get_inode fixes i_size at FILENT_SIZE (80), the width of
 *       the QSIZE:...NOTIFY_PID:... status line, not the live line length.
 */

#include "test_framework.h"

#include <fcntl.h>
#include <mqueue.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* Visual separator between sections; the framework only ships TEST_START and
 * the terminal TEST_DONE, so section ends print a rule of their own. */
#define TEST_END() printf("------------------------------------------------\n")

static const char *MQ_NAME = "/starry_mq_blockers";

static volatile sig_atomic_t g_notify_hits;

static void notify_handler(int sig)
{
    (void)sig;
    g_notify_hits++;
}

static void install_notify(int sig)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = notify_handler;
    sa.sa_flags = 0;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
}

static mqd_t open_fresh(long maxmsg)
{
    mq_unlink(MQ_NAME);
    struct mq_attr attr = {0};
    attr.mq_maxmsg = maxmsg;
    attr.mq_msgsize = 64;
    return mq_open(MQ_NAME, O_CREAT | O_RDWR, 0600, &attr);
}

static struct timespec deadline_in(int secs)
{
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    ts.tv_sec += secs;
    return ts;
}

/* ---- shared receiver-worker state ---- */
static mqd_t g_mq;
static volatile sig_atomic_t g_armed;
static long g_ret;
static int g_errno;

static void *recv_worker(void *arg)
{
    (void)arg;
    char buf[64];
    unsigned prio = 0;
    struct timespec ts = deadline_in(30);
    g_armed = 1;
    errno = 0;
    g_ret = mq_timedreceive(g_mq, buf, sizeof(buf), &prio, &ts);
    g_errno = errno;
    return NULL;
}

static void wait_parked(void)
{
    while (!g_armed)
        sched_yield();
    usleep(150 * 1000);
}

/*
 * B1: two sends land while a receiver is parked. send #1 is handed straight to
 * the receiver (queue stays empty); send #2 hits an empty queue with no waiter
 * and fires the registered notification exactly once.
 */
static void test_two_sends_notify(void)
{
    TEST_START("B1 two-sends-before-consume fires notify exactly once");

    g_mq = open_fresh(8);
    CHECK(g_mq != (mqd_t)-1, "mq_open for two-sends B1");
    if (g_mq == (mqd_t)-1)
        return;

    install_notify(SIGUSR2);
    struct sigevent sev = {0};
    sev.sigev_notify = SIGEV_SIGNAL;
    sev.sigev_signo = SIGUSR2;
    CHECK_RET(mq_notify(g_mq, &sev), 0, "mq_notify registers (B1)");

    g_notify_hits = 0;
    g_armed = 0;
    pthread_t th;
    pthread_create(&th, NULL, recv_worker, NULL);
    wait_parked();

    /* Two sends before the parked receiver is rescheduled. */
    CHECK_RET(mq_send(g_mq, "one", 3, 0), 0, "send #1 (handed to parked receiver)");
    CHECK_RET(mq_send(g_mq, "two", 3, 0), 0, "send #2 (empty queue, no waiter)");

    pthread_join(th, NULL);
    /* The receiver consumed exactly one message (the handed-off send #1). */
    CHECK(g_ret == 3 && g_errno == 0, "parked receiver got the handed-off message");

    usleep(150 * 1000); /* let any notification signal land */
    CHECK(g_notify_hits == 1,
          "notify fired exactly once (send #2 hit an empty queue after handoff)");

    /* Send #2 is still queued (curmsgs == 1); drain it. */
    struct mq_attr a;
    mq_getattr(g_mq, &a);
    CHECK(a.mq_curmsgs == 1, "exactly one message left queued after the handoff");
    char buf[64];
    (void)mq_receive(g_mq, buf, sizeof(buf), NULL);

    mq_close(g_mq);
    mq_unlink(MQ_NAME);
    TEST_END();
}

static const char *B2_LEAK_NAME = "/starry_mq_leak";

/* B2 child: cap RLIMIT_NOFILE low, exhaust the fd table with dup(), then
 * attempt to create a fresh named queue. Exit code:
 *   0  the create failed with EMFILE/ENFILE (fd allocation refused)
 *   1  the create unexpectedly succeeded, or failed with an unexpected errno
 *   2  setup problem (could not lower the limit / exhaust fds)
 * The queue name is deliberately NOT unlinked by the child: the parent then
 * checks the kernel-global name was not leaked by the failed create.
 */
static int b2_child(void)
{
    /* A small, deterministic fd ceiling regardless of the host/starry default. */
    struct rlimit rl = {.rlim_cur = 32, .rlim_max = 32};
    if (setrlimit(RLIMIT_NOFILE, &rl) != 0)
        return 2;

    int burned = 0;
    for (;;)
    {
        int fd = dup(1);
        if (fd == -1)
            break;
        burned++;
        if (burned > 1024)
            return 2; /* limit not honored */
    }
    if (errno != EMFILE)
        return 2;

    struct mq_attr attr = {0};
    attr.mq_maxmsg = 4;
    attr.mq_msgsize = 64;
    errno = 0;
    mqd_t q = mq_open(B2_LEAK_NAME, O_CREAT | O_EXCL | O_RDWR, 0600, &attr);
    if (q != (mqd_t)-1)
    {
        mq_close(q);
        return 1; /* fd table was exhausted yet the open somehow got an fd */
    }
    return (errno == EMFILE || errno == ENFILE) ? 0 : 1;
}

/*
 * B2: an mq_open that fails because the fd table is exhausted must not leak the
 * named queue. The child (small RLIMIT_NOFILE) drives the failed create; the
 * parent, with a healthy fd table, then verifies the name is absent - a fresh
 * O_CREAT|O_EXCL open of the same name must succeed (would be EEXIST if leaked).
 */
static void test_fd_exhaustion_no_leak(void)
{
    TEST_START("B2 mq_open fd-exhaustion leaves no leaked queue");

    mq_unlink(B2_LEAK_NAME);

    pid_t pid = fork();
    if (pid == 0)
        _exit(b2_child());
    int status = 0;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status), "B2 child exited normally");
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    CHECK(code == 0, "mq_open under fd exhaustion failed with EMFILE/ENFILE");

    /* If the failed open leaked the name, this O_EXCL open would see EEXIST. */
    struct mq_attr attr = {0};
    attr.mq_maxmsg = 4;
    attr.mq_msgsize = 64;
    errno = 0;
    mqd_t q2 = mq_open(B2_LEAK_NAME, O_CREAT | O_EXCL | O_RDWR, 0600, &attr);
    CHECK(q2 != (mqd_t)-1,
          "same name opens fresh with O_EXCL (no leaked queue from the failed open)");
    if (q2 != (mqd_t)-1)
    {
        mq_close(q2);
        mq_unlink(B2_LEAK_NAME);
    }
    TEST_END();
}

/* B3 helper: run in a setuid child. Returns exit code:
 *   0  write got EPERM as expected (and read still worked)
 *   1  write unexpectedly succeeded (or wrong errno)
 *   2  could not read the file / setup problem
 */
static int b3_child(void)
{
    const char *PATH = "/proc/sys/fs/mqueue/msg_max";

    /* Read must still work for the unprivileged process. */
    int rfd = open(PATH, O_RDONLY);
    if (rfd < 0)
        return 2;
    char rb[64];
    ssize_t r = read(rfd, rb, sizeof(rb) - 1);
    close(rfd);
    if (r <= 0)
        return 2;

    /* Write must be denied with EPERM. */
    int wfd = open(PATH, O_WRONLY);
    if (wfd < 0)
    {
        /* An open-for-write refused outright with EPERM/EACCES is also a
         * correct denial. */
        return (errno == EPERM || errno == EACCES) ? 0 : 1;
    }
    errno = 0;
    ssize_t w = write(wfd, "10\n", 3);
    int e = errno;
    close(wfd);
    if (w >= 0)
        return 1; /* write succeeded: not gated */
    return (e == EPERM) ? 0 : 1;
}

/*
 * B3: an unprivileged (non-CAP_SYS_RESOURCE) write to a mqueue sysctl is
 * denied with EPERM; reads stay open. Checked in a setuid child so the parent
 * stays root.
 */
static void test_sysctl_cap_gate(void)
{
    TEST_START("B3 /proc/sys/fs/mqueue write requires CAP_SYS_RESOURCE (EPERM)");

    if (access("/proc/sys/fs/mqueue/msg_max", F_OK) != 0)
    {
        CHECK(0, "/proc/sys/fs/mqueue/msg_max present");
        TEST_END();
        return;
    }

    if (geteuid() != 0)
    {
        /* Cannot drop privilege deterministically if not root; still assert the
         * write gate directly (we are already unprivileged). */
        int rc = b3_child();
        CHECK(rc == 0, "unprivileged write to msg_max denied with EPERM (read ok)");
        TEST_END();
        return;
    }

    pid_t pid = fork();
    if (pid == 0)
    {
        /* Drop to an unprivileged uid; setgroups/setgid first for good measure. */
        (void)setgid(65534);
        if (setuid(65534) != 0)
            _exit(2);
        _exit(b3_child());
    }
    int status = 0;
    waitpid(pid, &status, 0);
    CHECK(WIFEXITED(status), "B3 child exited normally");
    int code = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    CHECK(code == 0,
          "unprivileged write to msg_max denied with EPERM while read stays open");

    /* And root itself may still write (the gate is a privilege gate, not a
     * blanket read-only). Restore the value we read. */
    int wfd = open("/proc/sys/fs/mqueue/msg_max", O_WRONLY);
    if (wfd >= 0)
    {
        ssize_t w = write(wfd, "10\n", 3);
        CHECK(w == 3, "root (CAP_SYS_RESOURCE) may still write msg_max");
        close(wfd);
    }
    else
    {
        CHECK(0, "root can open msg_max for write");
    }
    TEST_END();
}

/*
 * B4: stat("/dev/mqueue/<name>") reports the fixed FILENT_SIZE (80), not the
 * live status-line length.
 */
static void test_devmqueue_size(void)
{
    TEST_START("B4 /dev/mqueue/<name> st_size == 80 (FILENT_SIZE)");

    if (access("/dev/mqueue", F_OK) != 0)
    {
        CHECK(0, "/dev/mqueue mounted");
        TEST_END();
        return;
    }

    mqd_t q = open_fresh(4);
    CHECK(q != (mqd_t)-1, "mq_open for B4");
    if (q == (mqd_t)-1)
    {
        TEST_END();
        return;
    }

    /* Name under /dev/mqueue has no leading slash. */
    char path[128];
    snprintf(path, sizeof(path), "/dev/mqueue%s", MQ_NAME);

    struct stat st;
    int rc = stat(path, &st);
    CHECK_RET(rc, 0, "stat /dev/mqueue/<name>");
    if (rc == 0)
        CHECK(st.st_size == 80, "st_size is the fixed FILENT_SIZE (80)");

    /* Put a message in the queue: the status line length changes but the fixed
     * inode size must NOT (it is not the live content length). */
    CHECK_RET(mq_send(q, "payload-bytes", 13, 0), 0, "send to change qsize");
    struct stat st2;
    if (stat(path, &st2) == 0)
        CHECK(st2.st_size == 80, "st_size stays 80 after qsize changes");

    char buf[64];
    (void)mq_receive(q, buf, sizeof(buf), NULL);
    mq_close(q);
    mq_unlink(MQ_NAME);
    TEST_END();
}

int main(void)
{
    test_two_sends_notify();
    test_fd_exhaustion_no_leak();
    test_sysctl_cap_gate();
    test_devmqueue_size();

    TEST_DONE();
}

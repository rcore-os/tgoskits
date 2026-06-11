/*
 * Multi-threaded execve regression test.
 *
 * Phase 0: `execve(path, NULL, NULL)` (raw syscall, libc bypassed) is a
 *          legitimate ABI shape on Linux. Resolving a nonexistent path must
 *          therefore reach path-resolution and yield ENOENT, never EFAULT.
 *          A successful exec of an existing file must also synthesize an
 *          empty-string argv[0], so the new image observes argc == 1 and
 *          argv[0] == "" instead of crashing during stack construction.
 * Phase 1: a failed execve from a multi-threaded process must leave the
 *          thread group intact (POSIX says execve failures preserve the
 *          process state).
 * Phase 2: a successful execve from a non-leader thread of a multi-threaded
 *          process must zap every other thread (including the original
 *          leader) and transfer the leader's TID/TGID identity to the
 *          calling thread, so the new image observes gettid() == getpid().
 *          We run this in a forked child so its successful exec doesn't
 *          consume the test driver before later phases run.
 * Phase 3 (pending-signal): a blocked, queued signal must survive execve
 *          (POSIX: the pending signal set is preserved across exec), and
 *          the disposition for a signal that had a custom user handler
 *          must be reset to SIG_DFL by execve. Run in a forked child:
 *          the child installs a custom SIGUSR1 handler, blocks SIGUSR1,
 *          raises a process-directed SIGUSR1 (queued because blocked),
 *          execs from a non-leader thread, and the new image asserts
 *          that SIGUSR1 is still pending *and* disposition is SIG_DFL
 *          before unblocking — at which point default-action Terminate
 *          delivers SIGUSR1 and kills the child. The driver waits and
 *          gates the phase on `WIFSIGNALED && WTERMSIG == SIGUSR1`.
 * Phase 4 (concurrent execve): when several sibling threads race to
 *          enter `execve` simultaneously, the kernel must serialize them
 *          so that a loser only fails when the winner has crossed into
 *          irreversible teardown. A `try_lock`-style serializer wrongly
 *          returns `EINTR` to the loser while the winner is still in the
 *          fallible path-resolve / ELF-load phase; if the winner then
 *          errors out, a loser that could have succeeded has been falsely
 *          aborted. We assert this by spawning N "bad-path execve spam"
 *          threads alongside one "good execve" thread: if the good thread
 *          ever observes `EINTR` from execve we trip a FAIL line; with the
 *          fix the good thread always either succeeds or sees a non-EINTR
 *          error.
 * Phase 5 (CLOEXEC race): an fd promoted to CLOEXEC by a sibling thread
 *          between the execve initiator's snapshot and the sibling's zap
 *          must still be closed in the new image. A pre-teardown snapshot
 *          could miss late `fcntl(F_SETFD, FD_CLOEXEC)` updates and leak
 *          fds across exec. We spawn a sibling that continuously toggles
 *          CLOEXEC on a fixed set of pipe fds and exec while the sibling
 *          is still running; the post-exec image then asserts every fd in
 *          the set is closed.
 * Phase 6 (vfork-blocked sibling vs execve): a sibling thread blocked in
 *          `wait_vfork_done` (its vfork child still alive) must be
 *          reapable by `zap_thread` when another thread does `execve`.
 *          The kernel wait must be killable; otherwise the execve
 *          initiator's sibling-teardown loop deadlocks waiting for the
 *          vfork-blocked sibling to exit. We arrange exactly this race
 *          (vfork child loops forever, a second thread does a good execve)
 *          and gate the phase on the new image's clean exit; a deadlock
 *          surfaces as a runner-timeout (no LEADER_CHILD_OK).
 * Phase 7 (robust-list owner-death after non-leader exec): when a
 *          non-leader thread of a multi-threaded process successfully
 *          execs, `Thread::tid()` is rebound to the leader's TGID so
 *          that `gettid() == getpid()` holds in the new image. The
 *          exit-time robust-futex walk (`handle_futex_death`) must
 *          compare the futex owner field against the *user-visible*
 *          TID (`Thread::tid()`), not the scheduler task id, otherwise
 *          a userspace robust mutex whose owner was written via
 *          `gettid()` in the post-exec image will never have its
 *          `FUTEX_OWNER_DIED` bit set on owner death, leaving every
 *          waiter parked forever. We run this in a forked child: a
 *          non-leader thread execs into `robust-list-child`; the new
 *          image installs a single-entry robust list whose futex word
 *          holds its own `gettid()`, spawns a waiter parked in
 *          `FUTEX_WAIT` on that word, and the main thread issues a
 *          raw `SYS_exit` (single-thread exit, not group_exit) so the
 *          robust-list walk fires. The waiter must observe
 *          `FUTEX_OWNER_DIED` set in the futex word.
 * Phase 8: a successful execve from the leader of a multi-threaded
 *          process must tear down the sibling threads and run the new
 *          image; the new image must observe gettid() == getpid(), and
 *          prints the final success marker LEADER_CHILD_OK. The runner's
 *          single-success regex matches only that marker — every earlier
 *          phase must pass for control to reach Phase 8.
 */

#include "test_framework.h"

#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int sibling_ready = 0;
static pthread_mutex_t mtx = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond = PTHREAD_COND_INITIALIZER;

#define SIBLING_SENTINEL ((void *)0xfeedbeefL)

static pid_t my_gettid(void) { return (pid_t)syscall(SYS_gettid); }

static void *sibling_quick(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&mtx);
    sibling_ready++;
    pthread_cond_broadcast(&cond);
    pthread_mutex_unlock(&mtx);

    /* Stay alive long enough for the main thread to run the bad execve.
     * 100 ms total is plenty without slowing the test down. */
    for (int i = 0; i < 100; i++) {
        struct timespec ts = { 0, 1000000 }; /* 1 ms */
        nanosleep(&ts, NULL);
    }
    return SIBLING_SENTINEL;
}

static void *sibling_block(void *arg)
{
    (void)arg;
    pthread_mutex_lock(&mtx);
    sibling_ready++;
    pthread_cond_broadcast(&cond);
    pthread_mutex_unlock(&mtx);

    /* Block until the exec path zaps us. */
    for (;;) {
        pause();
    }
    return NULL;
}

/* Non-leader thread that re-execs the test binary with a sentinel arg
 * exercising the de_thread leader-transfer path. The new image must
 * observe gettid() == getpid() — that's the crux of the assertion. */
static void *nonleader_exec_thread(void *arg)
{
    char *self = (char *)arg;
    char *av[] = { self, (char *)"nonleader-child", NULL };
    char *ev[] = { NULL };
    /* On success this never returns: the new image starts from main()
     * with our argv. If we ever fall through here, the exec failed and
     * the parent's pthread_join will see this thread return normally,
     * which the parent treats as failure. */
    execve(self, av, ev);
    return NULL;
}

/* Pre-exec SIGUSR1 handler installed by the pending-sig phase parent.
 * If this ever runs in the post-exec image, the kernel failed to reset
 * the disposition to SIG_DFL across execve — we surface that as a FAIL
 * line (the runner's fail_regex catches it) and exit non-signaled so the
 * parent sees a WIFEXITED with non-zero status. */
static void preexec_sigusr1_handler(int sig)
{
    (void)sig;
    fprintf(stderr,
            "FAIL: pre-exec SIGUSR1 handler ran in post-exec image\n");
    _exit(33);
}

/* Non-leader thread used by the pending-sig phase. Same shape as
 * `nonleader_exec_thread` but with the pending-sig sentinel. */
static void *nonleader_pending_sig_exec_thread(void *arg)
{
    char *self = (char *)arg;
    char *av[] = { self, (char *)"pending-sig-child", NULL };
    char *ev[] = { NULL };
    execve(self, av, ev);
    return NULL;
}

static void *assert_thread_inherits_blocked_sigusr1(void *arg)
{
    (void)arg;
    sigset_t now;
    sigemptyset(&now);
    if (sigprocmask(SIG_SETMASK, NULL, &now) != 0) {
        fprintf(stderr, "FAIL: child thread sigprocmask query: %s\n",
                strerror(errno));
        return (void *)43L;
    }
    if (!sigismember(&now, SIGUSR1)) {
        fprintf(stderr,
                "FAIL: pthread child did not inherit blocked SIGUSR1 mask\n");
        return (void *)44L;
    }
    return NULL;
}

static void assert_pthread_inherits_blocked_sigusr1(void)
{
    pthread_t th;
    if (pthread_create(&th, NULL, assert_thread_inherits_blocked_sigusr1,
                       NULL) != 0) {
        fprintf(stderr, "FAIL: spawn signal-mask inheritance thread\n");
        _exit(2);
    }

    void *result = NULL;
    if (pthread_join(th, &result) != 0) {
        fprintf(stderr, "FAIL: join signal-mask inheritance thread\n");
        _exit(2);
    }
    if (result != NULL) {
        fprintf(stderr,
                "FAIL: signal-mask inheritance thread returned %ld\n",
                (long)result);
        _exit(2);
    }
}

/* Phase 4 spam-thread body. Tries a bad-path execve in a tight loop so
 * it holds the per-process exec serializer briefly each iteration. The
 * goal is to maximize the chance that the racing "good execve" thread
 * encounters a held lock during its single attempt. The fix turns that
 * encounter into a wait; a regressed try_lock turns it into EINTR. */
static void *spam_bad_execve_thread(void *arg)
{
    (void)arg;
    char *av[] = { NULL };
    char *ev[] = { NULL };
    for (int i = 0; i < 5000; i++) {
        execve("/this/path/does/not/exist/mt-execve", av, ev);
        /* Bad path -> ENOENT. EINTR would mean we were zapped (the
         * good execve already committed). Either way, keep spinning;
         * we'll exit cleanly when the next syscall return hits
         * check_signals on user-return. */
    }
    return NULL;
}

/* Phase 4 good-execve thread. We must NOT observe EINTR: with the fix
 * an exec_lock loser waits for the holder to either fail-and-release
 * or commit-and-zap us. EINTR (from the lock itself) would mean the
 * regression is back. */
static void *good_execve_thread(void *arg)
{
    char *self = (char *)arg;
    char *av[] = { self, (char *)"nonleader-child", NULL };
    char *ev[] = { NULL };
    execve(self, av, ev);
    /* Only reached on failure. */
    if (errno == EINTR) {
        fprintf(stderr, "FAIL: good execve returned EINTR (concurrent race)\n");
    } else {
        fprintf(stderr, "FAIL: good execve failed errno=%d (%s)\n",
                errno, strerror(errno));
    }
    return NULL;
}

/* Phase 5 (CLOEXEC race). The setter thread continuously promotes a
 * fixed set of fds to CLOEXEC. The exec'ing main thread races ahead;
 * a pre-teardown snapshot of the FD table could miss late updates,
 * leaving non-closed fds in the new image. With the snapshot taken
 * post-teardown the setter's last-committed state is always observed. */
#define CLOEXEC_RACE_FDS 5
#define CLOEXEC_RACE_BASE_FD 20
static volatile int g_cloexec_setter_run;
static void *cloexec_setter_thread(void *arg)
{
    (void)arg;
    while (g_cloexec_setter_run) {
        for (int i = 0; i < CLOEXEC_RACE_FDS; i++) {
            (void)fcntl(CLOEXEC_RACE_BASE_FD + i, F_SETFD, FD_CLOEXEC);
        }
    }
    return NULL;
}

/* Re-entry body for Phase 5: verify every fd in the racing set is
 * closed (EBADF) in the new image. */
static int run_cloexec_check_child(void)
{
    int leaked = 0;
    for (int i = 0; i < CLOEXEC_RACE_FDS; i++) {
        int fd = CLOEXEC_RACE_BASE_FD + i;
        char buf;
        errno = 0;
        ssize_t n = read(fd, &buf, 1);
        if (n >= 0 || errno != EBADF) {
            fprintf(stderr,
                    "FAIL: fd %d still open after execve "
                    "(CLOEXEC leaked across exec, n=%zd errno=%d)\n",
                    fd, n, errno);
            leaked = 1;
        }
    }
    if (leaked) return 21;
    return 0;
}

/* Phase 7 (robust-list owner-death wake after non-leader exec).
 *
 * Layout matches the Linux `robust_list_head` / `robust_list` ABI: 24
 * bytes for the head (pointer, long, pointer), 8 bytes for each list
 * entry's `next` pointer. The kernel computes the futex word address
 * as `entry + futex_offset`, where `futex_offset` is in bytes from the
 * `next` field of the list entry.
 *
 * We use a static single-entry list. `g_robust_node.next` points back
 * to `&g_robust_head.list_first` (the kernel's `end_ptr` sentinel) so
 * the walk terminates after one entry. */
/* `<linux/futex.h>` provides these but we redefine them locally: the
 * loongarch64-linux-musl cross-toolchain in CI ships without that header
 * at all, and other targets may ship a stripped version. SYS_futex
 * itself comes from <sys/syscall.h>, which is portable. */
#ifndef FUTEX_WAIT
#define FUTEX_WAIT 0
#endif
#ifndef FUTEX_OWNER_DIED
#define FUTEX_OWNER_DIED 0x40000000u
#endif
#ifndef FUTEX_TID_MASK
#define FUTEX_TID_MASK 0x3fffffffu
#endif

struct robust_node {
    void    *next;
    uint32_t owner;
    uint32_t _pad;
};

struct robust_head {
    void *list_first;
    long  futex_offset;
    void *list_op_pending;
};

static struct robust_node g_robust_node;
static struct robust_head g_robust_head;

/* Waiter thread for the robust-list phase. Parks in FUTEX_WAIT on the
 * futex word; when the post-exec main thread exits, the kernel's robust
 * list walk must wake us and the word must have FUTEX_OWNER_DIED set.
 *
 * The fast-path `*uaddr != expected` short-circuit in FUTEX_WAIT means
 * that even if main exited before we parked we still observe the
 * OWNER_DIED bit on the post-wake read — both paths converge on the
 * same final assertion. A bounded 3s timeout converts a regression
 * (main exits without setting OWNER_DIED → wait never satisfied) into
 * an explicit FAIL line instead of a runner-level hang. */
static void *robust_waiter_thread(void *arg)
{
    uint32_t expected = (uint32_t)(uintptr_t)arg;
    struct timespec ft = { 3, 0 };
    long r = syscall(SYS_futex, &g_robust_node.owner, FUTEX_WAIT, expected,
                     &ft, NULL, 0);
    int saved_errno = errno;
    uint32_t val = g_robust_node.owner;
    if (!(val & FUTEX_OWNER_DIED)) {
        fprintf(stderr,
                "FAIL: FUTEX_OWNER_DIED bit not set after non-leader-exec "
                "main exit (val=%#x r=%ld errno=%d)\n",
                val, r, saved_errno);
        _exit(31);
    }
    _exit(0);
}

/* Re-entry body for Phase 7. We're already past the non-leader exec
 * (Phase 7's exec thread is what brought us here), so gettid() ==
 * getpid() must already hold; we re-assert it cheaply before doing
 * the robust-list work to keep the failure surface narrow. */
static int run_robust_list_child(void)
{
    pid_t tid = my_gettid();
    pid_t pid = getpid();
    if (tid != pid) {
        fprintf(stderr,
                "FAIL: post-exec gettid(%d) != getpid(%d) before robust-list test\n",
                (int)tid, (int)pid);
        return 41;
    }

    g_robust_node.next = &g_robust_head; /* end sentinel = &head.list_first */
    g_robust_node.owner = (uint32_t)tid & FUTEX_TID_MASK;
    g_robust_head.list_first = &g_robust_node;
    g_robust_head.futex_offset =
        (long)((char *)&g_robust_node.owner - (char *)&g_robust_node);
    g_robust_head.list_op_pending = NULL;

    if (syscall(SYS_set_robust_list, &g_robust_head,
                sizeof(g_robust_head)) != 0) {
        fprintf(stderr, "FAIL: set_robust_list: %s\n", strerror(errno));
        return 42;
    }

    pthread_t waiter;
    uintptr_t expected = (uintptr_t)((uint32_t)tid & FUTEX_TID_MASK);
    if (pthread_create(&waiter, NULL, robust_waiter_thread,
                       (void *)expected) != 0) {
        fprintf(stderr, "FAIL: spawn robust-list waiter thread\n");
        return 43;
    }

    /* Give the waiter time to enter FUTEX_WAIT. The OWNER_DIED bit gets
     * set regardless of ordering — see the comment in
     * `robust_waiter_thread` — but parking first exercises the actual
     * wake path that the reviewer asked for. */
    struct timespec ts = { 0, 50000000 }; /* 50ms */
    nanosleep(&ts, NULL);

    /* Raw SYS_exit: terminate this thread only (do_exit(_, false)),
     * not group_exit. Going through libc's exit() / _exit() would call
     * SYS_exit_group and kill the waiter before robust-list cleanup
     * could observe it. We also avoid musl's pthread_exit because some
     * libcs touch set_robust_list during thread teardown. */
    syscall(SYS_exit, 0);
    return 44; /* unreachable */
}

/* Non-leader thread used by the robust-list phase. Same shape as
 * `nonleader_exec_thread` / `nonleader_pending_sig_exec_thread` but
 * dispatches to the robust-list re-entry sentinel. */
static void *nonleader_robust_list_exec_thread(void *arg)
{
    char *self = (char *)arg;
    char *av[] = { self, (char *)"robust-list-child", NULL };
    char *ev[] = { NULL };
    execve(self, av, ev);
    return NULL;
}

/* Phase 6 (vfork-blocked sibling vs execve). The sibling thread calls
 * vfork(); the vfork child loops in pause() so the parent thread stays
 * blocked in `wait_vfork_done`. With the fix that wait is killable —
 * `zap_thread` will unblock it during another thread's execve teardown.
 * Without the fix the execve initiator hangs in its sibling-teardown
 * loop and we surface that as a runner timeout (no marker emitted). */
static void *vfork_blocker_thread(void *arg)
{
    (void)arg;
    pid_t v = vfork();
    if (v == 0) {
        /* vfork child: do not exit / exec so the parent thread stays
         * blocked in `wait_vfork_done`. The pause loop will end when
         * QEMU shuts down at end of test. */
        while (1) pause();
        _exit(0); /* unreachable */
    }
    /* Parent thread of vfork: blocked here until the child exits or
     * execs. Another thread of *our* process will execve and zap us;
     * our wait must wake on zap. If it does we fall through to return,
     * the syscall return path consumes `exit_request` and does
     * `do_exit(0, false)`. If it doesn't, we deadlock here. */
    return NULL;
}

static void wait_for_siblings(int n)
{
    pthread_mutex_lock(&mtx);
    while (sibling_ready < n) pthread_cond_wait(&cond, &mtx);
    pthread_mutex_unlock(&mtx);
}

/* Re-entry path used by both the non-leader and leader successful-exec
 * phases. The single invariant we check is the one Linux's de_thread
 * guarantees: the post-exec image is single-threaded and the calling
 * thread holds the original leader's identity, i.e.
 * gettid() == getpid(). */
static int run_post_exec_child(const char *marker)
{
    pid_t tid = my_gettid();
    pid_t pid = getpid();
    if (tid != pid) {
        fprintf(stderr,
                "FAIL: post-exec gettid(%d) != getpid(%d) (%s)\n",
                (int)tid, (int)pid, marker);
        return 1;
    }
    printf("%s\n", marker);
    return 0;
}

/* Re-entry path for the pending-signal phase. We arrive here as the
 * surviving (de_thread'd) thread of a fork-child whose pre-exec state
 * was: SIGUSR1 blocked, custom handler installed, one process-directed
 * SIGUSR1 raised and queued in the shared pending set.
 *
 * Linux requires that:
 *   1. pending signals (including those blocked at exec time) survive
 *      execve and remain pending against the new image,
 *   2. dispositions other than explicit SIG_IGN are reset to SIG_DFL
 *      (custom handlers point into the old aspace, so they must go),
 *   3. the per-thread blocked mask is preserved across exec.
 *
 * Unblocking SIGUSR1 then delivers the queued signal against the new
 * SIG_DFL disposition, whose default action is Terminate — so the child
 * terminates via SIGUSR1. The parent gates the phase on
 * WIFSIGNALED(status) && WTERMSIG(status) == SIGUSR1. */
static int run_pending_sig_child(void)
{
    /* (1) Pending set must still contain SIGUSR1. */
    sigset_t pending;
    sigemptyset(&pending);
    if (sigpending(&pending) != 0) {
        fprintf(stderr, "FAIL: sigpending: %s\n", strerror(errno));
        return 11;
    }
    if (!sigismember(&pending, SIGUSR1)) {
        fprintf(stderr, "FAIL: SIGUSR1 not pending after execve\n");
        return 12;
    }

    /* (2) Disposition must be SIG_DFL (handler was reset by execve). */
    struct sigaction cur;
    if (sigaction(SIGUSR1, NULL, &cur) != 0) {
        fprintf(stderr, "FAIL: sigaction query: %s\n", strerror(errno));
        return 13;
    }
    if (cur.sa_handler != SIG_DFL) {
        fprintf(stderr,
                "FAIL: SIGUSR1 disposition not SIG_DFL after execve "
                "(sa_handler=%p)\n", (void *)(uintptr_t)cur.sa_handler);
        return 14;
    }

    /* (3) Blocked mask must still contain SIGUSR1. */
    sigset_t now;
    sigemptyset(&now);
    if (sigprocmask(SIG_SETMASK, NULL, &now) != 0) {
        fprintf(stderr, "FAIL: sigprocmask query: %s\n", strerror(errno));
        return 15;
    }
    if (!sigismember(&now, SIGUSR1)) {
        fprintf(stderr,
                "FAIL: SIGUSR1 blocked mask not preserved across execve\n");
        return 16;
    }

    /* Unblock — must deliver SIGUSR1 immediately and terminate us. */
    sigset_t unblock;
    sigemptyset(&unblock);
    sigaddset(&unblock, SIGUSR1);
    if (sigprocmask(SIG_UNBLOCK, &unblock, NULL) != 0) {
        fprintf(stderr, "FAIL: sigprocmask unblock: %s\n", strerror(errno));
        return 17;
    }

    fprintf(stderr,
            "FAIL: SIGUSR1 not delivered after unblock (signal was lost)\n");
    return 18;
}

extern char **environ;

static int run_null_argv_child(int argc, char *argv[])
{
    int fail = 0;

    if (argc != 1) {
        fprintf(stderr, "FAIL: NULL argv child argc=%d, expected 1\n", argc);
        fail = 1;
    }
    if (argv == NULL || argv[0] == NULL) {
        fprintf(stderr, "FAIL: NULL argv child argv[0] is missing\n");
        fail = 1;
    } else if (argv[0][0] != '\0') {
        fprintf(stderr,
                "FAIL: NULL argv child argv[0]=%s, expected empty string\n",
                argv[0]);
        fail = 1;
    }
    if (argv != NULL && argc >= 1 && argv[1] != NULL) {
        fprintf(stderr, "FAIL: NULL argv child argv[1] is not NULL\n");
        fail = 1;
    }
    if (environ != NULL && environ[0] != NULL) {
        fprintf(stderr, "FAIL: NULL envp child environ[0] is not NULL\n");
        fail = 1;
    }

    if (fail) return 31;
    printf("NULL_ARGV_CHILD_OK\n");
    return 0;
}

int main(int argc, char *argv[])
{
    /* Unbuffered so phase markers reach the test runner before execve. */
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    /* Re-entry from the Phase 0 successful NULL argv/envp exec. */
    if (argc == 0
        || (argc == 1
            && (argv == NULL || argv[0] == NULL || argv[0][0] == '\0'))) {
        return run_null_argv_child(argc, argv);
    }

    /* Re-entry from Phase 2 (non-leader exec) inside the fork child. */
    if (argc >= 2 && strcmp(argv[1], "nonleader-child") == 0) {
        return run_post_exec_child("NONLEADER_CHILD_OK");
    }
    /* Re-entry from the pending-signal phase inside its fork child. */
    if (argc >= 2 && strcmp(argv[1], "pending-sig-child") == 0) {
        return run_pending_sig_child();
    }
    /* Re-entry from the CLOEXEC-race phase inside its fork child. */
    if (argc >= 2 && strcmp(argv[1], "cloexec-check-child") == 0) {
        return run_cloexec_check_child();
    }
    /* Re-entry from the robust-list owner-death phase inside its fork
     * child. The new image installs a robust list and a waiter, then
     * exits its main thread; passes iff the waiter observes
     * FUTEX_OWNER_DIED. */
    if (argc >= 2 && strcmp(argv[1], "robust-list-child") == 0) {
        return run_robust_list_child();
    }
    /* Re-entry from the final leader-exec phase. */
    if (argc >= 2 && strcmp(argv[1], "leader-child") == 0) {
        return run_post_exec_child("LEADER_CHILD_OK");
    }

    TEST_START("multi-thread execve");

    /* Phase 0: NULL argv/envp must reach path resolution, not be
     * short-circuited to EFAULT.
     *
     * Linux's `count_strings_kernel` (fs/exec.c) explicitly accepts a
     * NULL `argv.ptr.native` and treats it as an empty pointer array;
     * glibc's `execl(path, NULL)` and `execve(path, NULL, NULL)`
     * depend on that ABI. We don't actually want to consume the test
     * driver here, so we use a path that does not exist: success would
     * mean the kernel kept going past the NULL check and tried to
     * resolve the path, returning ENOENT — exactly what Linux does.
     * If the kernel had rejected NULL argv/envp with EFAULT we would
     * see EFAULT instead.
     *
     * Use the raw syscall so libc can't intercept and synthesize an
     * argv on our behalf. */
    errno = 0;
    long nullret = syscall(SYS_execve,
                           "/this/path/does/not/exist/mt-execve",
                           (char *const *)NULL, (char *const *)NULL);
    CHECK(nullret == -1L,
          "execve(path, NULL, NULL) to nonexistent path returns -1");
    CHECK(errno == ENOENT,
          "execve(path, NULL, NULL) returns ENOENT, not EFAULT");

    pid_t null_argv_pid = fork();
    CHECK(null_argv_pid != -1,
          "fork for successful execve(path, NULL, NULL) test");
    if (null_argv_pid == 0) {
        syscall(SYS_execve, "/usr/bin/test-mt-execve",
                (char *const *)NULL, (char *const *)NULL);
        fprintf(stderr,
                "FAIL: successful NULL argv/envp execve returned errno=%d (%s)\n",
                errno, strerror(errno));
        _exit(2);
    }

    int null_argv_status = 0;
    pid_t null_argv_waited = waitpid(null_argv_pid, &null_argv_status, 0);
    CHECK(null_argv_waited == null_argv_pid,
          "waitpid returned the NULL argv child");
    CHECK(WIFEXITED(null_argv_status),
          "NULL argv child exited normally");
    CHECK(WIFEXITED(null_argv_status) && WEXITSTATUS(null_argv_status) == 0,
          "NULL argv child observed argc=1 and empty argv[0]");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE0_OK\n");

    /* Phase 1: failed execve preserves thread group. */
    sibling_ready = 0;
    pthread_t qt1, qt2;
    CHECK(pthread_create(&qt1, NULL, sibling_quick, NULL) == 0,
          "spawn quick sibling 1");
    CHECK(pthread_create(&qt2, NULL, sibling_quick, NULL) == 0,
          "spawn quick sibling 2");
    wait_for_siblings(2);

    char *bad_argv[] = { (char *)"/this/path/does/not/exist/mt-execve", NULL };
    char *bad_envp[] = { NULL };
    errno = 0;
    int br = execve("/this/path/does/not/exist/mt-execve", bad_argv, bad_envp);
    CHECK(br == -1, "execve to nonexistent path returns -1");
    CHECK(errno == ENOENT,
          "errno is ENOENT after failed execve");

    void *r1 = NULL, *r2 = NULL;
    CHECK(pthread_join(qt1, &r1) == 0, "sibling 1 joinable after failed execve");
    CHECK(pthread_join(qt2, &r2) == 0, "sibling 2 joinable after failed execve");
    CHECK(r1 == SIBLING_SENTINEL, "sibling 1 returned its sentinel");
    CHECK(r2 == SIBLING_SENTINEL, "sibling 2 returned its sentinel");

    if (__fail > 0) {
        TEST_DONE();
    }

    /* Phase-1 marker. The success regex ANDs this with the post-exec markers
     * so the test only passes if all phases work. */
    printf("PHASE1_OK\n");

    /* Phase 2: non-leader thread successfully execs and inherits the
     * leader identity. We do this in a forked child so the success path
     * doesn't replace our test driver image and skip Phase 3.
     *
     * In the child:
     *   - spawn a couple of blocking siblings (so the leader is not the
     *     only co-thread; this exercises sibling teardown for both the
     *     blocking siblings and the original leader),
     *   - spawn a non-leader exec thread that calls execve to
     *     "nonleader-child",
     *   - the leader (main) sleeps; it'll be zapped by execve.
     *
     * The child's new image (from execve) prints NONLEADER_CHILD_OK and
     * exits 0. We assert child exit status to gate Phase 2 success. */
    pid_t cpid = fork();
    CHECK(cpid != -1, "fork for non-leader exec test");
    if (cpid == 0) {
        sibling_ready = 0;
        pthread_t bt1, bt2, nlt;
        if (pthread_create(&bt1, NULL, sibling_block, NULL) != 0
            || pthread_create(&bt2, NULL, sibling_block, NULL) != 0) {
            fprintf(stderr, "FAIL: spawn blocking sibling in fork child\n");
            _exit(2);
        }
        wait_for_siblings(2);

        if (pthread_create(&nlt, NULL, nonleader_exec_thread, argv[0]) != 0) {
            fprintf(stderr, "FAIL: spawn non-leader exec thread\n");
            _exit(2);
        }
        /* Wait to be zapped by the non-leader's successful exec. If exec
         * fails the non-leader thread returns normally; pthread_join
         * here would let us notice that. We give it 5 seconds before
         * giving up so a regression doesn't hang the test forever. */
        for (int i = 0; i < 5000; i++) {
            struct timespec ts = { 0, 1000000 };
            nanosleep(&ts, NULL);
        }
        fprintf(stderr, "FAIL: leader survived non-leader exec\n");
        _exit(3);
    }

    int wstatus = 0;
    pid_t waited = waitpid(cpid, &wstatus, 0);
    CHECK(waited == cpid, "waitpid returned the non-leader-exec child");
    CHECK(WIFEXITED(wstatus),
          "non-leader-exec child exited normally (not via signal)");
    CHECK(WEXITSTATUS(wstatus) == 0,
          "non-leader-exec child exited with status 0");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE2_OK\n");

    /* Phase 3 (pending-signal): blocked queued signal survives execve,
     * custom handler is reset to SIG_DFL across exec. Run in a forked
     * child whose pre-exec setup installs a handler, blocks SIGUSR1, and
     * raises a process-directed SIGUSR1 that lands in the shared pending
     * set; a non-leader thread then execs. The post-exec image verifies
     * pending+disposition+blocked-mask and unblocks, at which point the
     * default-action Terminate kills the child with SIGUSR1. */
    pid_t spid = fork();
    CHECK(spid != -1, "fork for pending-signal exec test");
    if (spid == 0) {
        struct sigaction sa;
        memset(&sa, 0, sizeof(sa));
        sa.sa_handler = preexec_sigusr1_handler;
        sigemptyset(&sa.sa_mask);
        if (sigaction(SIGUSR1, &sa, NULL) != 0) {
            fprintf(stderr, "FAIL: install pre-exec SIGUSR1 handler: %s\n",
                    strerror(errno));
            _exit(2);
        }

        sigset_t block;
        sigemptyset(&block);
        sigaddset(&block, SIGUSR1);
        if (sigprocmask(SIG_BLOCK, &block, NULL) != 0) {
            fprintf(stderr, "FAIL: block SIGUSR1 pre-exec: %s\n",
                    strerror(errno));
            _exit(2);
        }
        assert_pthread_inherits_blocked_sigusr1();

        /* Process-directed signal lands in the shared pending set
         * (POSIX) and so doesn't depend on which thread eventually
         * survives de_thread. */
        if (kill(getpid(), SIGUSR1) != 0) {
            fprintf(stderr, "FAIL: queue SIGUSR1: %s\n", strerror(errno));
            _exit(2);
        }

        pthread_t bt1, bt2, et;
        if (pthread_create(&bt1, NULL, sibling_block, NULL) != 0
            || pthread_create(&bt2, NULL, sibling_block, NULL) != 0) {
            fprintf(stderr, "FAIL: spawn blocking siblings (pending-sig)\n");
            _exit(2);
        }
        wait_for_siblings(2);
        sibling_ready = 0;

        if (pthread_create(&et, NULL, nonleader_pending_sig_exec_thread,
                           argv[0]) != 0) {
            fprintf(stderr, "FAIL: spawn pending-sig exec thread\n");
            _exit(2);
        }
        for (int i = 0; i < 5000; i++) {
            struct timespec ts = { 0, 1000000 };
            nanosleep(&ts, NULL);
        }
        fprintf(stderr, "FAIL: leader survived pending-sig non-leader exec\n");
        _exit(3);
    }

    int sstatus = 0;
    pid_t swaited = waitpid(spid, &sstatus, 0);
    CHECK(swaited == spid, "waitpid returned the pending-sig-exec child");
    CHECK(WIFSIGNALED(sstatus),
          "pending-sig-exec child terminated by a signal "
          "(unblocked SIGUSR1 was delivered)");
    CHECK(WIFSIGNALED(sstatus) && WTERMSIG(sstatus) == SIGUSR1,
          "pending-sig-exec child terminated by SIGUSR1 specifically");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE3_OK\n");

    /* Phase 4: concurrent execve (bad-path spam + one good execve).
     * With the exec_lock fix the good thread must never observe EINTR
     * from execve itself: it waits for the spammer to release the lock
     * (which the spammer does at every failed iteration) and then either
     * acquires the lock and succeeds, or is zapped by another thread
     * that commits ahead of it. */
    pid_t crpid = fork();
    CHECK(crpid != -1, "fork for concurrent-exec test");
    if (crpid == 0) {
        pthread_t spam[3];
        for (size_t i = 0; i < sizeof(spam) / sizeof(spam[0]); i++) {
            if (pthread_create(&spam[i], NULL, spam_bad_execve_thread,
                               NULL) != 0) {
                fprintf(stderr, "FAIL: spawn bad-exec spam thread\n");
                _exit(2);
            }
        }
        /* Let the spammers spin up a bit so they hold the lock for
         * meaningful portions of the good thread's wait window. */
        struct timespec ramp = { 0, 10000000 }; /* 10ms */
        nanosleep(&ramp, NULL);

        pthread_t gt;
        if (pthread_create(&gt, NULL, good_execve_thread, argv[0]) != 0) {
            fprintf(stderr, "FAIL: spawn good-exec thread\n");
            _exit(2);
        }
        /* Wait to be zapped by the good thread's successful exec. */
        for (int i = 0; i < 5000; i++) {
            struct timespec ts = { 0, 1000000 };
            nanosleep(&ts, NULL);
        }
        fprintf(stderr, "FAIL: concurrent-exec child leader survived\n");
        _exit(3);
    }

    int crstatus = 0;
    pid_t crwaited = waitpid(crpid, &crstatus, 0);
    CHECK(crwaited == crpid, "waitpid returned the concurrent-exec child");
    CHECK(WIFEXITED(crstatus),
          "concurrent-exec child exited normally (not via signal)");
    CHECK(WIFEXITED(crstatus) && WEXITSTATUS(crstatus) == 0,
          "concurrent-exec child exited with status 0");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE4_OK\n");

    /* Phase 5: CLOEXEC race. A sibling thread continuously promotes a
     * fixed set of pipe-read fds to CLOEXEC while the main thread races
     * ahead into execve. With the snapshot taken *after* sibling
     * teardown the new image must see every racing fd closed. */
    pid_t cxpid = fork();
    CHECK(cxpid != -1, "fork for cloexec-race test");
    if (cxpid == 0) {
        for (int i = 0; i < CLOEXEC_RACE_FDS; i++) {
            int p[2];
            if (pipe(p) != 0) {
                fprintf(stderr, "FAIL: pipe(): %s\n", strerror(errno));
                _exit(2);
            }
            int target = CLOEXEC_RACE_BASE_FD + i;
            if (dup2(p[0], target) == -1) {
                fprintf(stderr,
                        "FAIL: dup2(p[0]=%d, %d): %s\n",
                        p[0], target, strerror(errno));
                _exit(2);
            }
            close(p[0]);
            close(p[1]);
            /* Make sure CLOEXEC starts unset so the sibling has work
             * to do (and so a regression that snapshots before
             * teardown could miss the late update). */
            int fl = fcntl(target, F_GETFD);
            if (fl == -1 || (fl & FD_CLOEXEC)) {
                fprintf(stderr,
                        "FAIL: pre-race fd %d has FD_CLOEXEC set\n",
                        target);
                _exit(2);
            }
        }

        g_cloexec_setter_run = 1;
        pthread_t st;
        if (pthread_create(&st, NULL, cloexec_setter_thread, NULL) != 0) {
            fprintf(stderr, "FAIL: spawn cloexec setter thread\n");
            _exit(2);
        }

        /* Give the setter a moment to start fcntl'ing, then race
         * straight into execve. */
        struct timespec ramp = { 0, 1000000 }; /* 1ms */
        nanosleep(&ramp, NULL);

        char *av[] = { argv[0], (char *)"cloexec-check-child", NULL };
        char *ev[] = { NULL };
        execve(argv[0], av, ev);
        fprintf(stderr, "FAIL: cloexec-race execve returned errno=%d (%s)\n",
                errno, strerror(errno));
        _exit(3);
    }

    int cxstatus = 0;
    pid_t cxwaited = waitpid(cxpid, &cxstatus, 0);
    CHECK(cxwaited == cxpid, "waitpid returned the cloexec-race child");
    CHECK(WIFEXITED(cxstatus),
          "cloexec-race child exited normally (not via signal)");
    CHECK(WIFEXITED(cxstatus) && WEXITSTATUS(cxstatus) == 0,
          "cloexec-race child exited with status 0");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE5_OK\n");

    /* Phase 6: a sibling thread blocked in `wait_vfork_done` must be
     * unblocked when another thread's execve zaps it. Without the fix
     * the wait sleeps on a non-interruptible primitive and the execve
     * initiator's sibling-teardown loop deadlocks; that surfaces as
     * the runner-level timeout for this whole test rather than a
     * waitpid-observable failure. */
    pid_t vfpid = fork();
    CHECK(vfpid != -1, "fork for vfork-deadlock test");
    if (vfpid == 0) {
        pthread_t blocker;
        if (pthread_create(&blocker, NULL, vfork_blocker_thread, NULL) != 0) {
            fprintf(stderr, "FAIL: spawn vfork blocker thread\n");
            _exit(2);
        }
        /* Give the blocker enough time to enter `wait_vfork_done`. */
        struct timespec ramp = { 0, 50000000 }; /* 50ms */
        nanosleep(&ramp, NULL);

        pthread_t et;
        if (pthread_create(&et, NULL, nonleader_exec_thread, argv[0]) != 0) {
            fprintf(stderr, "FAIL: spawn vfork-deadlock exec thread\n");
            _exit(2);
        }
        for (int i = 0; i < 5000; i++) {
            struct timespec ts = { 0, 1000000 };
            nanosleep(&ts, NULL);
        }
        fprintf(stderr, "FAIL: vfork-deadlock child leader survived\n");
        _exit(3);
    }

    int vfstatus = 0;
    pid_t vfwaited = waitpid(vfpid, &vfstatus, 0);
    CHECK(vfwaited == vfpid, "waitpid returned the vfork-deadlock child");
    CHECK(WIFEXITED(vfstatus),
          "vfork-deadlock child exited normally (not via signal)");
    CHECK(WIFEXITED(vfstatus) && WEXITSTATUS(vfstatus) == 0,
          "vfork-deadlock child exited with status 0");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE6_OK\n");

    /* Phase 7: robust-list owner-death wake after non-leader execve.
     *
     * Regression test for `handle_futex_death` using `Thread::tid()`
     * (= user-visible TID, what `gettid()` returns) instead of the
     * scheduler task id. Pre-fix: after a non-leader exec the two
     * diverged, the owner-field comparison silently mismatched, the
     * `FUTEX_OWNER_DIED` bit was never set, and robust-mutex waiters
     * stayed parked forever. We drive that exact scenario in a fork
     * child: a non-leader thread execs into `robust-list-child`, the
     * new image installs a robust list and a waiter, then raw-SYS_exits
     * its main thread; the waiter must observe `FUTEX_OWNER_DIED` in
     * the futex word (it has a 3s timeout that converts a regression
     * into an explicit FAIL line instead of a runner-level hang). */
    pid_t rlpid = fork();
    CHECK(rlpid != -1, "fork for robust-list owner-death test");
    if (rlpid == 0) {
        pthread_t bt1, bt2, et;
        if (pthread_create(&bt1, NULL, sibling_block, NULL) != 0
            || pthread_create(&bt2, NULL, sibling_block, NULL) != 0) {
            fprintf(stderr,
                    "FAIL: spawn blocking siblings (robust-list)\n");
            _exit(2);
        }
        wait_for_siblings(2);
        sibling_ready = 0;

        if (pthread_create(&et, NULL, nonleader_robust_list_exec_thread,
                           argv[0]) != 0) {
            fprintf(stderr, "FAIL: spawn robust-list exec thread\n");
            _exit(2);
        }
        for (int i = 0; i < 5000; i++) {
            struct timespec ts = { 0, 1000000 };
            nanosleep(&ts, NULL);
        }
        fprintf(stderr,
                "FAIL: robust-list child leader survived non-leader exec\n");
        _exit(3);
    }

    int rlstatus = 0;
    pid_t rlwaited = waitpid(rlpid, &rlstatus, 0);
    CHECK(rlwaited == rlpid, "waitpid returned the robust-list child");
    CHECK(WIFEXITED(rlstatus),
          "robust-list child exited normally (not via signal)");
    CHECK(WIFEXITED(rlstatus) && WEXITSTATUS(rlstatus) == 0,
          "robust-list child exited with status 0 "
          "(waiter saw FUTEX_OWNER_DIED)");

    if (__fail > 0) {
        TEST_DONE();
    }

    printf("PHASE7_OK\n");

    /* Phase 8: successful execve from the leader of a multi-threaded
     * process. Siblings block in pause() and are zapped during sibling
     * teardown; control then jumps into the new image. */
    sibling_ready = 0;
    pthread_t lt1, lt2, lt3;
    CHECK(pthread_create(&lt1, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 1");
    CHECK(pthread_create(&lt2, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 2");
    CHECK(pthread_create(&lt3, NULL, sibling_block, NULL) == 0,
          "spawn blocking sibling 3");
    wait_for_siblings(3);

    if (__fail > 0) {
        TEST_DONE();
    }

    char *good_argv[] = { argv[0], (char *)"leader-child", NULL };
    char *good_envp[] = { NULL };
    execve(argv[0], good_argv, good_envp);

    /* Should not be reached on success. */
    fprintf(stderr, "FAIL: leader execve returned, errno=%d (%s)\n",
            errno, strerror(errno));
    return 1;
}

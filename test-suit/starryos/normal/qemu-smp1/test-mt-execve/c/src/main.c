/*
 * Multi-threaded execve regression test.
 *
 * Phase 0: `execve(path, NULL, NULL)` (raw syscall, libc bypassed) is a
 *          legitimate ABI shape on Linux — `count_strings_kernel`
 *          accepts a NULL argv/envp and treats it as an empty pointer
 *          array. Resolving a nonexistent path must therefore reach
 *          path-resolution and yield ENOENT, never EFAULT.
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
 * Phase 4: a successful execve from the leader of a multi-threaded
 *          process must tear down the sibling threads and run the new
 *          image; the new image must observe gettid() == getpid(), and
 *          prints the final success marker LEADER_CHILD_OK. The runner's
 *          single-success regex matches only that marker — every earlier
 *          phase must pass for control to reach Phase 4.
 */

#include "test_framework.h"

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

int main(int argc, char *argv[])
{
    /* Unbuffered so phase markers reach the test runner before execve. */
    setvbuf(stdout, NULL, _IONBF, 0);
    setvbuf(stderr, NULL, _IONBF, 0);

    /* Re-entry from Phase 2 (non-leader exec) inside the fork child. */
    if (argc >= 2 && strcmp(argv[1], "nonleader-child") == 0) {
        return run_post_exec_child("NONLEADER_CHILD_OK");
    }
    /* Re-entry from the pending-signal phase inside its fork child. */
    if (argc >= 2 && strcmp(argv[1], "pending-sig-child") == 0) {
        return run_pending_sig_child();
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

    /* Phase 4: successful execve from the leader of a multi-threaded
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

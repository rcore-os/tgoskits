/*
 * bug-zombie-syscalls.c
 *
 * Verifies that getsid(), getpgid(), and getpriority(PRIO_PROCESS) return
 * correct values for zombie processes, and return ESRCH after waitpid() reaps
 * them.
 *
 * Linux semantics (verified via man pages and live test):
 *   - A zombie still occupies the process table.
 *   - getsid(zombie)              → session ID (same as parent's)
 *   - getpgid(zombie)             → process group ID (same as parent's)
 *   - getpriority(PRIO_PROCESS, zombie) → exited leader's nice value
 *   - After waitpid(): all three  → -1 / ESRCH
 *
 * Synchronization:
 *   waitid(WNOWAIT|WNOHANG) observes that the child is waitable without
 *   reaping it.  The bounded loop avoids hanging the whole QEMU case if child
 *   exit notification regresses.
 */

#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int passed = 0;
static int failed = 0;
static atomic_bool worker_ready = ATOMIC_VAR_INIT(false);
static atomic_bool leader_departing = ATOMIC_VAR_INIT(false);

#define CHECK(cond, msg)                                                       \
    do {                                                                       \
        if (cond) {                                                            \
            printf("  PASS: %s\n", (msg));                                     \
            passed++;                                                          \
        } else {                                                               \
            printf("  FAIL: %s  (errno=%d: %s)\n", (msg), errno,              \
                   strerror(errno));                                           \
            failed++;                                                          \
        }                                                                      \
    } while (0)

static void *last_thread_main(void *arg)
{
    (void)arg;
    if (setpriority(PRIO_PROCESS, 0, 12) != 0)
        _exit(101);

    atomic_store_explicit(&worker_ready, true, memory_order_release);
    while (!atomic_load_explicit(&leader_departing, memory_order_acquire))
        sched_yield();

    /*
     * Keep the worker alive until the leader has completed do_exit(). The
     * worker's distinct nice value makes a missing retired-leader snapshot
     * observable when this thread becomes the last process member.
     */
    struct timespec delay = { .tv_sec = 0, .tv_nsec = 100000000 };
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR)
        ;
    return NULL;
}

static void run_multithreaded_child(void)
{
    if (setpriority(PRIO_PROCESS, 0, 7) != 0)
        _exit(100);

    pthread_t worker;
    if (pthread_create(&worker, NULL, last_thread_main, NULL) != 0)
        _exit(102);

    while (!atomic_load_explicit(&worker_ready, memory_order_acquire))
        sched_yield();
    atomic_store_explicit(&leader_departing, true, memory_order_release);
    pthread_exit(NULL);
}

static int wait_until_zombie(pid_t child)
{
    for (int waited_us = 0; waited_us < 5000000; waited_us += 10000) {
        siginfo_t info;
        memset(&info, 0, sizeof(info));
        if (waitid(P_PID, (id_t)child, &info,
                   WEXITED | WNOWAIT | WNOHANG) == 0) {
            if (info.si_pid == child)
                return 0;
        } else if (errno != EINTR) {
            return -1;
        }
        struct timespec ts = { .tv_sec = 0, .tv_nsec = 10000000 };
        nanosleep(&ts, NULL);
    }
    errno = ETIMEDOUT;
    return -1;
}

int main(void)
{
    /* Record parent's sid and pgid — zombie child must return the same. */
    pid_t parent_sid  = getsid(0);
    pid_t parent_pgid = getpgid(0);

    printf("parent pid=%d  sid=%d  pgid=%d\n",
           (int)getpid(), (int)parent_sid, (int)parent_pgid);

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }

    if (child == 0) {
        run_multithreaded_child();
        _exit(103);
    }

    if (wait_until_zombie(child) != 0) {
        perror("waitid(WNOWAIT) for zombie child");
        (void)kill(child, SIGKILL);
        (void)waitpid(child, NULL, WNOHANG);
        return EXIT_FAILURE;
    }

    printf("\n--- checks before waitpid (zombie state) ---\n");

    /* getsid(zombie) must return the session ID, not ESRCH. */
    errno = 0;
    pid_t sid = getsid(child);
    CHECK(sid == parent_sid,
          "getsid(zombie) == parent sid");

    /* getpgid(zombie) must return the process group ID, not ESRCH. */
    errno = 0;
    pid_t pgid = getpgid(child);
    CHECK(pgid == parent_pgid,
          "getpgid(zombie) == parent pgid");

    /*
     * The leader exited before the final worker. The zombie must retain the
     * leader's nice value (7), not the last worker's value (12).
     */
    errno = 0;
    int prio = getpriority(PRIO_PROCESS, (id_t)child);
    CHECK(errno == 0 && prio == 7,
          "getpriority(PRIO_PROCESS, zombie) retains leader nice");

    /* Reap the child. */
    int status;
    pid_t waited = waitpid(child, &status, 0);
    CHECK(waited == child, "waitpid() returns child pid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exited with status 0");

    printf("\n--- checks after waitpid (reaped) ---\n");

    /* getsid(reaped) must return ESRCH. */
    errno = 0;
    sid = getsid(child);
    CHECK(sid == (pid_t)-1, "getsid(reaped) == -1");
    CHECK(errno == ESRCH,   "getsid(reaped) sets errno=ESRCH");

    /* getpgid(reaped) must return ESRCH. */
    errno = 0;
    pgid = getpgid(child);
    CHECK(pgid == (pid_t)-1, "getpgid(reaped) == -1");
    CHECK(errno == ESRCH,    "getpgid(reaped) sets errno=ESRCH");

    /* getpriority(PRIO_PROCESS, reaped) must return ESRCH. */
    errno = 0;
    prio = getpriority(PRIO_PROCESS, (id_t)child);
    CHECK(prio == -1 && errno == ESRCH,
          "getpriority(PRIO_PROCESS, reaped) sets errno=ESRCH");

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

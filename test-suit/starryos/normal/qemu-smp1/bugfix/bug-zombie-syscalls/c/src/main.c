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
 *   - getpriority(PRIO_PROCESS, zombie) → 0  (nice value, not ESRCH)
 *   - After waitpid(): all three  → -1 / ESRCH
 *
 * Synchronization:
 *   waitid(WNOWAIT|WNOHANG) observes that the child is waitable without
 *   reaping it.  The bounded loop avoids hanging the whole QEMU case if child
 *   exit notification regresses.
 */

#include <errno.h>
#include <signal.h>
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
        _exit(0);
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

    /* getpriority(PRIO_PROCESS, zombie) must return 0, not ESRCH. */
    errno = 0;
    int prio = getpriority(PRIO_PROCESS, (id_t)child);
    CHECK(errno == 0 && prio == 0,
          "getpriority(PRIO_PROCESS, zombie) == 0");

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

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
 * Synchronization (same pattern as bug-kill-zombie-esrch):
 *   The child writes a byte to a pipe just before _exit().  The parent reads
 *   it to know the child has called _exit(), then sleeps 10 ms to let
 *   process.exit() complete (which sets the zombie state).  Only then are
 *   the zombie checks performed.
 */

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <sys/wait.h>
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

int main(void)
{
    /* Record parent's sid and pgid — zombie child must return the same. */
    pid_t parent_sid  = getsid(0);
    pid_t parent_pgid = getpgid(0);

    printf("parent pid=%d  sid=%d  pgid=%d\n",
           (int)getpid(), (int)parent_sid, (int)parent_pgid);

    /* Pipe for synchronization: child writes 1 byte before _exit(). */
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        perror("pipe");
        return EXIT_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }

    if (child == 0) {
        /* ---- child ---- */
        close(pipefd[0]);
        /* Signal parent that we are about to exit. */
        char byte = 1;
        (void)write(pipefd[1], &byte, 1);
        close(pipefd[1]);
        _exit(0);
    }

    /* ---- parent ---- */
    close(pipefd[1]);

    /* Wait for child's write — child is about to call _exit(). */
    char byte;
    (void)read(pipefd[0], &byte, 1);
    close(pipefd[0]);

    /*
     * Sleep 10 ms so do_exit() can finish process.exit() and register the
     * zombie.  The gap between pipe-close and zombie registration is tiny
     * but real; the sleep makes the test reliable.
     */
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 10000000 }; /* 10 ms */
    nanosleep(&ts, NULL);

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

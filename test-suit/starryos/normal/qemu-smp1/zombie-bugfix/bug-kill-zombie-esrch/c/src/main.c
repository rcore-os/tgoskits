/*
 * bug-kill-zombie-esrch.c
 *
 * Verifies POSIX/Linux kill(pid, 0) semantics across the zombie lifecycle:
 *
 *   1. kill(zombie_pid, 0) == 0        -- zombie exists, not yet reaped
 *   2. kill(reaped_pid,  0) == ESRCH   -- process gone after waitpid()
 *
 * Previous bug in StarryOS: send_signal_to_process() returned ESRCH for
 * zombie processes, violating POSIX (man 2 kill NOTE: "an existing process
 * might be a zombie").  A separate earlier bug returned 0 after reap.
 *
 * Synchronization:
 *   waitpid(WNOHANG) reaps atomically when it finds a zombie, so it cannot be
 *   used to wait for "zombie but not reaped" state.  Instead poll waitid()
 *   with WNOWAIT|WNOHANG: it observes that the child is waitable without
 *   consuming the zombie.  The loop is bounded so a scheduler/process bug
 *   reports a test failure instead of hanging the whole QEMU case.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int passed;
static int failed;

#define CHECK(cond, msg)                                                \
    do {                                                                \
        if (cond) {                                                     \
            printf("  [OK]   %s\n", (msg));                            \
            passed++;                                                   \
        } else {                                                        \
            printf("  [FAIL] %s (errno=%d %s)\n",                      \
                   (msg), errno, strerror(errno));                      \
            failed++;                                                   \
        }                                                               \
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
        usleep(10000);
    }
    errno = ETIMEDOUT;
    return -1;
}

int main(void)
{
    printf("=== bug-kill-zombie-esrch ===\n");

    /*
     * Ignore SIGCHLD so the kernel does not auto-reap the zombie and
     * so the child does not disappear before we can observe it.
     * SIG_DFL on Linux means "reap automatically" for SIGCHLD when
     * SA_NOCLDWAIT is set; SIG_IGN prevents auto-reap on most kernels
     * but POSIX allows either.  We use SIG_DFL here and rely on the
     * explicit waitpid() to reap.
     */
    signal(SIGCHLD, SIG_DFL);

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

    /*
     * --- check 1: kill(zombie, 0) must return 0 ---
     *
     * The child is now a zombie (exited, not yet reaped).
     * POSIX: "An existing process might be a zombie" — kill(pid,0) must
     * return 0, not ESRCH.
     */
    errno = 0;
    int rc = kill(child, 0);
    CHECK(rc == 0,
          "kill(zombie_child, 0) == 0  [zombie exists, not yet reaped]");

    /* --- check 2: reap the child --- */
    int status;
    pid_t waited = waitpid(child, &status, 0);
    CHECK(waited == child, "waitpid() returns child pid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exited with status 0");

    /* --- check 3: kill(reaped, 0) must return ESRCH --- */
    errno = 0;
    rc = kill(child, 0);
    CHECK(rc == -1,
          "kill(reaped_child, 0) == -1  [process no longer exists]");
    CHECK(errno == ESRCH,
          "kill(reaped_child, 0) sets errno=ESRCH");

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

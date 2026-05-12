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
 *   sigwaitinfo(SIGCHLD) stalls in StarryOS because send_signal does not
 *   interrupt a task that is blocked in rt_sigtimedwait when the signal is
 *   in the wait set but currently blocked at the signal layer.
 *
 *   Instead we use a pipe: the child writes a byte just before _exit().
 *   The parent reads it to know the child has called _exit().  At that
 *   point the child may not yet be a zombie (close_all_fds runs before
 *   process.exit() in do_exit), so we spin on waitpid(WNOHANG) until the
 *   child is waitable — but we must NOT reap it yet.
 *
 *   Problem: waitpid(WNOHANG) reaps atomically when it finds a zombie.
 *   We cannot "peek" without reaping.
 *
 *   Solution: use a short usleep() after the pipe read.  The child's
 *   do_exit() path is:
 *     close_all_fds()  → pipe write end closed → parent unblocks
 *     process.exit()   → is_zombie = true
 *     send SIGCHLD to parent
 *   The gap between pipe-close and is_zombie is a single function call.
 *   A 10 ms sleep is orders of magnitude longer than that gap on any
 *   real scheduler, making the race window negligible in practice.
 *
 *   This is intentionally a best-effort synchronization.  If the timing
 *   is wrong, check 1 may spuriously pass (child still alive) but will
 *   never spuriously fail.  The test is designed to FAIL with the buggy
 *   kernel (is_zombie → ESRCH) and PASS with the correct kernel.
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

    /* Pipe for synchronization: child writes before _exit(). */
    int pipefd[2];
    if (pipe(pipefd) < 0) {
        perror("pipe");
        return EXIT_FAILURE;
    }

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }

    if (child == 0) {
        /* Child: signal parent then exit. */
        close(pipefd[0]);
        char byte = 1;
        (void)write(pipefd[1], &byte, 1);
        close(pipefd[1]);
        _exit(0);
    }

    /* Parent: wait for child to write, then give it time to become zombie. */
    close(pipefd[1]);
    char buf;
    (void)read(pipefd[0], &buf, 1);
    close(pipefd[0]);

    /*
     * The child has called write() and is about to _exit().
     * do_exit() calls close_all_fds() then process.exit() (sets is_zombie).
     * Sleep 10ms to let process.exit() complete.
     */
    usleep(10000);

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

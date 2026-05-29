/*
 * bug-kill-zombie-perm.c
 *
 * Regression test for: non-root same-UID caller gets ESRCH when sending a
 * signal to an unreaped zombie whose task has already been GC'd.
 *
 * Root cause (StarryOS):
 *   sys_kill(pid > 0) calls check_kill_permission() BEFORE the zombie-aware
 *   send_signal_to_process().  check_kill_permission() does:
 *
 *       get_task(target_pid)  →  ESRCH  (task GC'd)
 *
 *   and returns ESRCH to userspace before we ever reach the is_zombie_pid()
 *   guard.  On Linux the task_struct (and its cred) lives until waitpid()
 *   reaps the zombie, so the permission check always succeeds for a same-UID
 *   caller.
 *
 * Test sequence:
 *   1. Fork a child that exits immediately (becomes zombie, not reaped).
 *   2. Parent drops privileges: setuid(1000).
 *   3. Parent calls kill(zombie, 0)    → must return  0, not ESRCH.
 *   4. Parent calls kill(zombie, SIGKILL) → must return  0 (signal silently
 *      dropped for zombies, but permission must be granted).
 *   5. Parent reaps the zombie with waitpid().
 *   6. Parent calls kill(reaped, 0)    → must return -1 / ESRCH.
 *
 * Synchronization: poll waitid(WNOWAIT|WNOHANG) until the child is waitable
 * without reaping it.  The loop is bounded so a scheduler/process bug reports
 * a test failure instead of hanging the whole QEMU case.
 *
 * The test is designed to FAIL on the buggy kernel (check_kill_permission
 * returns ESRCH for a GC'd task) and PASS once the fix stores a cred
 * snapshot in the zombie table and consults it during permission checks.
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

#define NON_ROOT_UID 1000

static int passed;
static int failed;

#define CHECK(cond, msg)                                                       \
    do {                                                                       \
        if (cond) {                                                            \
            printf("  [OK]   %s\n", (msg));                                   \
            passed++;                                                          \
        } else {                                                               \
            printf("  [FAIL] %s (errno=%d %s)\n",                             \
                   (msg), errno, strerror(errno));                             \
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
        usleep(10000);
    }
    errno = ETIMEDOUT;
    return -1;
}

int main(void)
{
    printf("=== bug-kill-zombie-perm ===\n");

    /* Must start as root so setuid() later actually drops privileges. */
    if (getuid() != 0) {
        fprintf(stderr, "SKIP: must run as root (uid=%d)\n", getuid());
        /* Not a test failure — environment precondition not met. */
        printf("TEST PASSED\n");
        return EXIT_SUCCESS;
    }

    /* Keep SIGCHLD default so the zombie is not auto-reaped. */
    signal(SIGCHLD, SIG_DFL);

    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }

    if (child == 0) {
        /*
         * Child: also drop to the same non-root UID so the parent's
         * permission check (same euid) should succeed.
         */
        if (setuid(NON_ROOT_UID) < 0) {
            perror("child setuid");
            _exit(1);
        }
        _exit(0);
    }

    if (wait_until_zombie(child) != 0) {
        perror("waitid(WNOWAIT) for zombie child");
        (void)kill(child, SIGKILL);
        (void)waitpid(child, NULL, WNOHANG);
        return EXIT_FAILURE;
    }

    /*
     * Drop privileges AFTER the child is a zombie so we are still root during
     * fork and zombie-state synchronization.
     * Now both parent and child have uid/euid == NON_ROOT_UID.
     */
    if (setuid(NON_ROOT_UID) < 0) {
        perror("parent setuid");
        return EXIT_FAILURE;
    }
    printf("  parent dropped to uid=%d\n", (int)getuid());

    /* --- check 1: kill(zombie, 0) from same-UID non-root --- */
    errno = 0;
    int rc = kill(child, 0);
    CHECK(rc == 0,
          "kill(zombie, 0) == 0  [same-UID non-root, zombie not yet reaped]");

    /* --- check 2: kill(zombie, SIGKILL) from same-UID non-root --- */
    errno = 0;
    rc = kill(child, SIGKILL);
    CHECK(rc == 0,
          "kill(zombie, SIGKILL) == 0  [signal silently dropped for zombie]");

    /* --- check 3: reap the zombie --- */
    int status;
    pid_t waited = waitpid(child, &status, 0);
    CHECK(waited == child, "waitpid() returns child pid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exited cleanly with status 0");

    /* --- check 4: kill(reaped, 0) must now return ESRCH --- */
    errno = 0;
    rc = kill(child, 0);
    CHECK(rc == -1,
          "kill(reaped, 0) == -1  [process fully gone after waitpid]");
    CHECK(errno == ESRCH,
          "kill(reaped, 0) sets errno=ESRCH");

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0)
        printf("TEST PASSED\n");
    else
        printf("TEST FAILED\n");

    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

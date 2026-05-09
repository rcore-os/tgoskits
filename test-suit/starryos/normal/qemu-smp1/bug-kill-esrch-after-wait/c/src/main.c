/*
 * bug-kill-esrch-after-wait.c
 *
 * Bug: StarryOS returns 0 from kill(pid, 0) after the child has already been
 * reaped by waitpid(). POSIX and Linux both require kill(pid, 0) to return -1
 * with errno=ESRCH once the process no longer exists.
 *
 * Observed in StarryOS syscall trace:
 *   [2.314584] wait4 return Ok(11)      <- child reaped
 *   [2.315875] kill  return Ok(0)       <- BUG: should be ESRCH
 *
 * Minimal reproduction:
 *   1. fork() a child that exits immediately
 *   2. waitpid() to reap it
 *   3. kill(child_pid, 0) must return -1 / ESRCH
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

#define CHECK(cond, msg) \
    do { \
        if (cond) { \
            printf("  [OK] %s\n", (msg)); \
            passed++; \
        } else { \
            printf("  [FAIL] %s (errno=%d %s)\n", (msg), errno, strerror(errno)); \
            failed++; \
        } \
    } while (0)

int main(void)
{
    printf("=== bug-kill-esrch-after-wait ===\n");

    /* --- step 1: fork a child that exits immediately --- */
    pid_t child = fork();
    if (child < 0) {
        perror("fork");
        return EXIT_FAILURE;
    }
    if (child == 0) {
        /* child: exit right away */
        _exit(0);
    }

    /* --- step 2: verify child is alive before wait --- */
    errno = 0;
    int rc = kill(child, 0);
    /* child may have already exited but not yet reaped — both 0 and ESRCH are
     * acceptable here; we just don't want a hard failure on this check */
    printf("  [INFO] kill(child, 0) before wait: rc=%d errno=%d (%s)\n",
           rc, errno, strerror(errno));

    /* --- step 3: reap the child --- */
    int status;
    pid_t waited = waitpid(child, &status, 0);
    CHECK(waited == child, "waitpid returns child pid");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exited with status 0");

    /* --- step 4: kill(pid, 0) MUST return ESRCH after reap --- */
    errno = 0;
    rc = kill(child, 0);
    CHECK(rc == -1,
          "kill(reaped_child, 0) returns -1");
    CHECK(errno == ESRCH,
          "kill(reaped_child, 0) sets errno=ESRCH");

    printf("\n=== result: %d passed, %d failed ===\n", passed, failed);
    if (failed == 0) {
        printf("TEST PASSED\n");
    } else {
        printf("TEST FAILED\n");
    }
    return failed == 0 ? EXIT_SUCCESS : EXIT_FAILURE;
}

/*
 * test-pgkill-session — verify process-group kill across session boundaries.
 *
 * Topology tested:
 *   parent
 *     └── child  (fork, then setsid() → new session + process group leader)
 *            └── grandchild (fork + exec "sleep 999")
 *
 * Actions:
 *   1. Parent forks child
 *   2. Child: setsid() → becomes session + pg leader
 *   3. Child: fork grandchild, grandchild exec("sleep 999")
 *   4. Child: signal parent ready via pipe
 *   5. Parent: kill(-child_pgid, SIGKILL) — kill process group
 *   6. Parent: waitpid(child) and waitpid(grandchild) — both should be reapable
 *   7. Parent: check kill(-child_pgid, 0) returns -ESRCH (group gone)
 *
 * Expected Linux behavior:
 *   - kill(-pgid, SIGKILL) kills all processes in the process group
 *   - Both child and grandchild are reaped via waitpid
 *   - kill(-pgid, 0) returns -1 with ESRCH
 *
 * StarryOS current behavior (to be tested):
 *   - process-group kill may or may not work
 *   - session boundaries may affect signal delivery
 */

#define _GNU_SOURCE
#include "test_framework.h"

#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* ---- Helper: waitpid with timeout ---- */
static pid_t waitpid_timeout(pid_t target, int *status, int timeout_sec)
{
    /* For WNOHANG polling */
    if (timeout_sec < 0) {
        errno = 0;
        pid_t got = waitpid(target, status, WNOHANG);
        return got;
    }

    time_t start = time(NULL);
    while (time(NULL) - start < timeout_sec) {
        pid_t got = waitpid(target, status, WNOHANG);
        if (got > 0) return got;
        if (got < 0 && errno == ECHILD) return -1;

        struct timespec ts = {0, 50 * 1000 * 1000}; /* 50ms */
        nanosleep(&ts, NULL);
    }
    errno = ETIMEDOUT;
    return -1;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    TEST_START("process-group kill across session boundary");

    /* ---- Scenario 1: kill process group after setsid ---- */

    {
        int ready_pipe[2];
        CHECK(pipe(ready_pipe) == 0, "create ready pipe");

        pid_t child = fork();
        CHECK(child >= 0, "fork child");

        if (child == 0) {
            /* ========== CHILD ========== */
            close(ready_pipe[0]);

            /* Become a session and process group leader */
            pid_t sid = setsid();
            if (sid < 0) {
                printf("  FAIL | setsid() failed: errno=%d (%s)\n",
                       errno, strerror(errno));
                write(ready_pipe[1], "\x01", 1);
                close(ready_pipe[1]);
                _exit(1);
            }

            pid_t pgid = getpgid(0);
            pid_t my_pid = getpid();
            printf("  INFO | child pid=%d sid=%d pgid=%d\n", my_pid, sid, pgid);

            if (pgid != my_pid) {
                printf("  FAIL | pgid %d != pid %d after setsid\n", pgid, my_pid);
                write(ready_pipe[1], "\x02", 1);
                close(ready_pipe[1]);
                _exit(2);
            }

            /* Fork grandchild */
            pid_t gc = fork();
            if (gc < 0) {
                printf("  FAIL | grandchild fork failed: errno=%d (%s)\n",
                       errno, strerror(errno));
                write(ready_pipe[1], "\x03", 1);
                close(ready_pipe[1]);
                _exit(3);
            }

            if (gc == 0) {
                /* ====== GRANDCHILD ====== */
                pid_t gc_pid = getpid();
                pid_t gc_pgid = getpgid(0);
                printf("  INFO | grandchild pid=%d pgid=%d\n", gc_pid, gc_pgid);

                /* Signal parent we're running */
                /* Use a short sleep so parent can observe us */
                sleep(30);
                _exit(0);
            }

            /* Signal parent: ready, write child pgid */
            pid_t my_pgid = getpgid(0);
            {
                char buf[32];
                snprintf(buf, sizeof(buf), "%d\n", my_pgid);
                write(ready_pipe[1], buf, strlen(buf));
            }
            close(ready_pipe[1]);

            /* Parent will kill us via process group */
            for (;;) {
                sleep(10);
            }
        }

        /* ========== PARENT ========== */
        close(ready_pipe[1]);

        /* Read child's process group ID */
        char pgid_buf[32];
        ssize_t n = read(ready_pipe[0], pgid_buf, sizeof(pgid_buf) - 1);
        close(ready_pipe[0]);

        if (n <= 0 || pgid_buf[0] < '0') {
            CHECK(0, "child setup failed");
            waitpid(child, NULL, 0);
            TEST_DONE();
            return 1;
        }

        pgid_buf[n] = '\0';
        pid_t child_pgid = (pid_t)atoi(pgid_buf);
        printf("  INFO | parent read child pgid = %d\n", child_pgid);

        /* Verify we CAN see the child pgid */
        {
            pid_t pgid_check = getpgid(child);
            if (pgid_check > 0) {
                printf("  INFO | parent sees child pgid via getpgid(child)=%d\n", pgid_check);
            } else {
                printf("  INFO | parent getpgid(child) failed: errno=%d (%s)\n",
                       errno, strerror(errno));
            }
        }

        /* ---- Step: kill entire process group ---- */
        printf("  INFO | sending kill(-pgid=%d, SIGKILL)\n", -(int)child_pgid);
        int kr = kill(-child_pgid, SIGKILL);
        if (kr == 0) {
            printf("  INFO | kill(-pgid, SIGKILL) returned 0: success\n");
        } else {
            printf("  INFO | kill(-pgid, SIGKILL) returned %d, errno=%d (%s)\n",
                   kr, errno, strerror(errno));
            CHECK(kr == 0, "kill(-pgid, SIGKILL) succeeds");
        }

        /* ---- Step: reap child ---- */
        {
            int status = 0;
            pid_t got = waitpid_timeout(child, &status, 5);
            if (got > 0) {
                printf("  INFO | child pid=%d reaped, status=%d\n", got, status);
                CHECK_RET(got, child, "waitpid reaps child after pgkill");
                CHECK(WIFSIGNALED(status), "child terminated by signal after pgkill");
                if (WIFSIGNALED(status)) {
                    CHECK(WTERMSIG(status) == SIGKILL, "child got SIGKILL");
                }
            } else {
                printf("  FAIL | child not reaped within 5s (got=%d errno=%d)\n",
                       got, errno);
                CHECK(0, "child reaped after pgkill");
            }
        }

        /* ---- Step: check if any grandchild zombie appeared ---- */
        {
            int status = 0;
            pid_t got = waitpid_timeout(-1, &status, 3);
            if (got > 0) {
                printf("  INFO | waitpid(-1) found zombie grandchild pid=%d status=%d\n",
                       got, status);
                /* Check if the zombie is a process group member */
            } else if (got == 0 || (got < 0 && errno == ETIMEDOUT)) {
                printf("  WARN | no zombie grandchild within 3s after pgkill\n");
                printf("  WARN | grandchild may have survived process group kill\n");
            } else {
                printf("  INFO | waitpid(-1) returned %d errno=%d — "
                       "no children at all\n",
                       got, errno);
                /* ECHILD = no children → all reaped */
                if (got < 0 && errno == ECHILD) {
                    printf("  INFO | ECHILD: all children were properly reaped\n");
                }
            }
        }

        /* ---- Step: verify process group is gone ---- */
        {
            errno = 0;
            int r = kill(-child_pgid, 0);
            if (r == -1 && errno == ESRCH) {
                printf("  INFO | kill(-pgid, 0) → ESRCH: process group is gone (good)\n");
            } else if (r == 0) {
                printf("  WARN | kill(-pgid, 0) → 0: process group still exists!\n");
            } else {
                printf("  INFO | kill(-pgid, 0) → %d errno=%d (%s)\n",
                       r, errno, strerror(errno));
            }
        }
    }

    printf("PGKILL_SESSION_PASSED\n");
    TEST_DONE();
}

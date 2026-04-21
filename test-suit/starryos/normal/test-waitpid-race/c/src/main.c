/*
 * test-waitpid-race
 *
 * Exercises the sys_waitpid register-before-check race.
 *
 * Race: if the waiter first checks for reapable children and then registers
 * on the wait queue, a child that exits and sends SIGCHLD between the check
 * and the register loses its wake-up. The parent parks forever.
 *
 * We fork many short-lived children in a tight loop. Each iteration the child
 * exits immediately with a known status and the parent waits for it. If the
 * race exists at least one iteration will hang and the whole test trips the
 * harness timeout. If it does not, all iterations finish and we validate that
 * every reaped child carried the expected exit status.
 */

#include "test_framework.h"
#include <sys/wait.h>
#include <unistd.h>

#define ITERATIONS 500
#define EXIT_CODE 37

int main(void)
{
    TEST_START("waitpid race: fork -> immediate _exit -> waitpid");

    int ok = 1;
    for (int i = 0; i < ITERATIONS; i++) {
        pid_t pid = fork();
        if (pid < 0) {
            CHECK(pid >= 0, "fork");
            ok = 0;
            break;
        }
        if (pid == 0) {
            _exit(EXIT_CODE);
        }

        int status = 0;
        pid_t w;
        do {
            w = waitpid(pid, &status, 0);
        } while (w == -1 && errno == EINTR);

        if (w != pid) {
            CHECK(w == pid, "waitpid returns child pid");
            ok = 0;
            break;
        }
        if (!WIFEXITED(status) || WEXITSTATUS(status) != EXIT_CODE) {
            CHECK(WIFEXITED(status) && WEXITSTATUS(status) == EXIT_CODE,
                  "child exit status matches");
            ok = 0;
            break;
        }
    }

    CHECK(ok, "all iterations completed without losing a wake");

    TEST_DONE();
}

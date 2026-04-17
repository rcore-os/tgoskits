/*
 * test_prctl_pdeathsig.c
 *
 * 测试 prctl PR_SET_PDEATHSIG / PR_GET_PDEATHSIG 完整功能。
 * 包括信号存储、查询、清除，以及父进程退出时实际投递信号。
 * PostgreSQL 子进程 (checkpointer, bgwriter 等) 依赖此机制。
 */

#include "test_framework.h"
#include <sys/prctl.h>
#include <signal.h>
#include <unistd.h>
#include <sys/wait.h>

static volatile sig_atomic_t got_signal = 0;
static int report_fd = -1;

static void sigusr1_handler(int sig)
{
    (void)sig;
    got_signal = 1;
    char c = 1;
    write(report_fd, &c, 1);
    _exit(0);
}

int main(void)
{
    TEST_START("prctl_pdeathsig: PR_SET_PDEATHSIG / PR_GET_PDEATHSIG");

    /* Test 1: PR_SET_PDEATHSIG should succeed */
    CHECK_RET(prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0), 0,
              "PR_SET_PDEATHSIG(SIGTERM)");

    /* Test 2: PR_GET_PDEATHSIG should return the set value */
    {
        int sig = -1;
        int rc = prctl(PR_GET_PDEATHSIG, (unsigned long)&sig, 0, 0, 0);
        CHECK(rc == 0, "PR_GET_PDEATHSIG succeeds");
        CHECK(sig == SIGTERM, "PR_GET_PDEATHSIG returns SIGTERM");
    }

    /* Test 3: PR_SET_PDEATHSIG with signal 0 (clear) */
    CHECK_RET(prctl(PR_SET_PDEATHSIG, 0, 0, 0, 0), 0,
              "PR_SET_PDEATHSIG(0) clears");

    /* Test 4: After clearing, PR_GET_PDEATHSIG returns 0 */
    {
        int sig = -1;
        prctl(PR_GET_PDEATHSIG, (unsigned long)&sig, 0, 0, 0);
        CHECK(sig == 0, "after clear, PR_GET_PDEATHSIG returns 0");
    }

    /* Test 5: Invalid signal number rejected */
    {
        int rc = prctl(PR_SET_PDEATHSIG, 65, 0, 0, 0);
        CHECK(rc == -1, "PR_SET_PDEATHSIG(65) rejected");
    }

    /* Test 6: Signal delivered on parent death.
     *
     * Grandparent (us) -> parent -> child.
     * Child sets PR_SET_PDEATHSIG(SIGUSR1).
     * Parent exits. Kernel delivers SIGUSR1 to child.
     * Child writes result to a pipe that grandparent reads. */
    {
        int ready_pipe[2]; /* child -> parent: "I'm set up" */
        int result_pipe[2]; /* child -> grandparent: "got signal" */
        pipe(ready_pipe);
        pipe(result_pipe);

        pid_t mid = fork();
        if (mid == 0) {
            /* Middle process: fork child, wait for ready, then exit */
            close(result_pipe[0]);

            pid_t child = fork();
            if (child == 0) {
                /* Grandchild */
                close(ready_pipe[0]);
                close(result_pipe[0]);
                report_fd = result_pipe[1];

                struct sigaction sa = {0};
                sa.sa_handler = sigusr1_handler;
                sigaction(SIGUSR1, &sa, NULL);

                prctl(PR_SET_PDEATHSIG, SIGUSR1, 0, 0, 0);

                /* Tell parent we're ready */
                write(ready_pipe[1], "r", 1);
                close(ready_pipe[1]);

                /* Wait for signal; 2s timeout = failure */
                sleep(2);
                char c = 0;
                write(result_pipe[1], &c, 1);
                _exit(1);
            }
            close(ready_pipe[1]);
            close(result_pipe[1]);
            /* Wait for child to set up, then exit */
            char buf;
            read(ready_pipe[0], &buf, 1);
            close(ready_pipe[0]);
            _exit(0);
        }

        /* Grandparent */
        close(ready_pipe[0]);
        close(ready_pipe[1]);
        close(result_pipe[1]);

        /* Wait for middle process to exit */
        int status;
        waitpid(mid, &status, 0);

        /* Read result from grandchild */
        alarm(5);
        char result = 0;
        ssize_t n = read(result_pipe[0], &result, 1);
        alarm(0);
        close(result_pipe[0]);

        CHECK(n == 1 && result == 1,
              "pdeathsig delivered on parent exit");

        while (waitpid(-1, NULL, WNOHANG) > 0) {}
    }

    TEST_DONE();
}

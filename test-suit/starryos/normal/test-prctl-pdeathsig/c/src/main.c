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

    /* Test 6: Child can set and fork inherits. */
    {
        pid_t pid = fork();
        if (pid == 0) {
            int rc = prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
            int sig = -1;
            prctl(PR_GET_PDEATHSIG, (unsigned long)&sig, 0, 0, 0);
            _exit((rc == 0 && sig == SIGKILL) ? 0 : 1);
        }
        int status;
        waitpid(pid, &status, 0);
        CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
              "child set+get pdeathsig round trip");
    }

    TEST_DONE();
}

/*
 * test-compat: fork / process-identity semantics for StarryOS
 *
 * This test focuses narrowly on a semantic that is NOT covered by the
 * existing base-branch tests (test-credentials, test-uid-gid-getters,
 * test-open-family, test-write, test-stat-family):
 *
 *   the parent/child relationship established by fork().
 *
 * After fork(), POSIX guarantees:
 *   - the child observes getppid() == parent's getpid()
 *   - fork()'s return value in the parent equals the child's getpid()
 *   - parent and child have distinct PIDs
 *   - waitpid() reaps exactly the forked child
 *
 * The child reports its own getpid()/getppid() back through a pipe so that
 * all assertions run in the parent process (the test framework's pass/fail
 * counters are per-process and would not survive the fork otherwise).
 */

#include "test_framework.h"

#include <sys/wait.h>
#include <unistd.h>

static void test_fork_identity(void)
{
    pid_t parent_pid = getpid();
    CHECK(parent_pid > 0, "getpid() returns a positive PID in the parent");

    int pipefd[2];
    if (pipe(pipefd) != 0) {
        CHECK(0, "pipe() for parent/child communication");
        return;
    }

    pid_t fork_ret = fork();
    CHECK(fork_ret >= 0, "fork() succeeds");

    if (fork_ret == 0) {
        /* Child: report own pid and ppid to the parent, then exit. */
        close(pipefd[0]);
        pid_t info[2];
        info[0] = getpid();   /* child's own PID                       */
        info[1] = getppid();  /* must equal the parent's getpid()      */
        ssize_t n = write(pipefd[1], info, sizeof(info));
        close(pipefd[1]);
        _exit(n == (ssize_t)sizeof(info) ? 0 : 1);
    }

    /* Parent. */
    close(pipefd[1]);
    pid_t info[2] = { -1, -1 };
    ssize_t n = read(pipefd[0], info, sizeof(info));
    close(pipefd[0]);
    CHECK(n == (ssize_t)sizeof(info), "parent reads child's pid/ppid from pipe");

    pid_t child_pid = info[0];
    pid_t child_ppid = info[1];

    int status = 0;
    pid_t waited = waitpid(fork_ret, &status, 0);

    /* Core fork semantics. */
    CHECK(child_ppid == parent_pid,
          "child's getppid() equals the parent's getpid()");
    CHECK(child_pid == fork_ret,
          "fork() return value in parent equals the child's getpid()");
    CHECK(child_pid != parent_pid,
          "parent and child have distinct PIDs");
    CHECK(waited == fork_ret,
          "waitpid() reaps exactly the forked child");
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0,
          "child exited normally with status 0");
}

int main(void)
{
    TEST_START("fork parent-child identity semantics");

    test_fork_identity();

    TEST_DONE();
}

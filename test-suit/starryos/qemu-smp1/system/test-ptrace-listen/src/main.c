/*
 * test-ptrace-listen
 *
 * Verify PTRACE_LISTEN semantics per man 2 ptrace:
 *   - Fails with ESRCH on a non-traced process.
 *   - Fails with ESRCH on a SEIZE'd but still-running tracee.
 *   - After a group-stop (SIGSTOP): LISTEN releases the ptrace-stop and
 *     puts the tracee into a job-control stop.  A subsequent SIGCONT
 *     resumes the tracee and waitpid(WCONTINUED) reports the continue.
 */

#include "test_framework.h"
#include <signal.h>
#include <sys/ptrace.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef PTRACE_SEIZE
#define PTRACE_SEIZE 0x4206
#endif
#ifndef PTRACE_LISTEN
#define PTRACE_LISTEN 0x4208
#endif

static void kill_and_reap(pid_t pid)
{
    kill(pid, SIGKILL);
    waitpid(pid, NULL, 0);
}

/* --- test 1: LISTEN on non-traced child returns ESRCH --- */
static int test_listen_not_traced(void)
{
    TEST_START("LISTEN on non-traced child returns ESRCH");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid < 0) TEST_DONE();
    if (pid == 0) {
        pause();
        _exit(0);
    }

    CHECK_ERR(ptrace(PTRACE_LISTEN, pid, NULL, NULL), ESRCH,
              "LISTEN on non-traced child");

    kill_and_reap(pid);
    TEST_DONE();
}

/* --- test 2: LISTEN on SEIZE'd but running tracee returns ESRCH --- */
static int test_listen_running_seized(void)
{
    TEST_START("LISTEN on running SEIZE'd tracee returns ESRCH");

    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid < 0) TEST_DONE();
    if (pid == 0) {
        pause();
        _exit(0);
    }

    CHECK_RET(ptrace(PTRACE_SEIZE, pid, NULL, NULL), 0, "ptrace SEIZE");
    CHECK_ERR(ptrace(PTRACE_LISTEN, pid, NULL, NULL), ESRCH,
              "LISTEN on running seized tracee");

    kill_and_reap(pid);
    TEST_DONE();
}

/* --- test 3: LISTEN after group-stop, SIGCONT resumes --- */
static int test_listen_group_stop_then_cont(void)
{
    TEST_START("LISTEN after group-stop, then SIGCONT resumes");

    int sync_pipe[2];
    CHECK(pipe(sync_pipe) == 0, "pipe");
    if (sync_pipe[0] < 0) {
        /* pipe failed; CHECK already recorded the failure */
        TEST_DONE();
    }

    pid_t pid = fork();
    CHECK(pid >= 0, "fork");
    if (pid < 0) {
        close(sync_pipe[0]); close(sync_pipe[1]);
        TEST_DONE();
    }

    if (pid == 0) {
        /* child side */
        close(sync_pipe[0]);
        char r = 1;
        if (write(sync_pipe[1], &r, 1) != 1) _exit(99);  /* signal ready   */
        if (read(sync_pipe[1], &r, 1) != 1)  _exit(98);   /* wait for SEIZE */
        close(sync_pipe[1]);
        raise(SIGSTOP);  /* enter group-stop */
        _exit(42);
    }

    /* parent side */
    close(sync_pipe[1]);

    /* wait for child to signal ready */
    {
        char r;
        CHECK(read(sync_pipe[0], &r, 1) == 1, "sync read from child");
    }

    CHECK_RET(ptrace(PTRACE_SEIZE, pid, NULL, NULL), 0, "ptrace SEIZE");

    /* tell child to raise(SIGSTOP) */
    {
        char r = 1;
        CHECK(write(sync_pipe[0], &r, 1) == 1, "sync write to child");
    }
    close(sync_pipe[0]);

    /* wait for the group-stop notification */
    {
        int status = 0;
        pid_t got = waitpid(pid, &status, WUNTRACED);
        CHECK(got == pid, "waitpid returns pid after SIGSTOP");
        if (got == pid) {
            CHECK(WIFSTOPPED(status), "WIFSTOPPED");
            if (WIFSTOPPED(status)) {
                CHECK(WSTOPSIG(status) == SIGSTOP, "WSTOPSIG == SIGSTOP");
            }
        }
    }

    /* LISTEN — release ptrace-stop, enter job-control stop */
    CHECK_RET(ptrace(PTRACE_LISTEN, pid, NULL, NULL), 0, "PTRACE_LISTEN");

    /* After LISTEN the tracee is NOT in ptrace-stop.  Pass a non-null
     * data pointer so the kernel reaches the ptrace-stopped check
     * (null data -> EINVAL, which would mask the ESRCH we want). */
    {
        struct riscv_user_regs { unsigned long r[32]; } regs;
        CHECK_ERR(ptrace(PTRACE_GETREGS, pid, NULL, &regs), ESRCH,
                  "GETREGS fails after LISTEN (not in ptrace-stop)");
    }

    /* SIGCONT resumes the tracee from job-control stop */
    CHECK_RET(kill(pid, SIGCONT), 0, "kill SIGCONT");

    {
        int status = 0;
        pid_t got = waitpid(pid, &status, WCONTINUED);
        CHECK(got == pid, "waitpid WCONTINUED returns pid");
        if (got == pid) {
            CHECK(WIFCONTINUED(status), "WIFCONTINUED");
        }
    }

    /* After SIGCONT the child resumes from raise(SIGSTOP) and exits */
    {
        int status = 0;
        pid_t got = waitpid(pid, &status, 0);
        CHECK(got == pid, "waitpid reaps exited child");
        if (got == pid) {
            CHECK(WIFEXITED(status), "WIFEXITED");
            if (WIFEXITED(status)) {
                CHECK(WEXITSTATUS(status) == 42, "WEXITSTATUS == 42");
            }
        }
    }

    TEST_DONE();
}

int main(void)
{
    test_listen_not_traced();
    test_listen_running_seized();
    test_listen_group_stop_then_cont();
    return __fail > 0 ? 1 : 0;
}
